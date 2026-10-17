//! `ganymede`: drive kernels from the command line. Milestone 1 of the sketch:
//! launch a kernel from its kernelspec, run one cell, print what came back.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ganymede_kernel_host::protocol::content::{self, ExecuteRequest};
use ganymede_kernel_host::protocol::{Channel, Message};
use ganymede_kernel_host::{
    attach, in_process_pair, process::read_connection_file, zmq::ZmqTransport, FsKernelspecs,
    KernelSession, KernelspecSource, LaunchOptions, Provisioner, SubprocessProvisioner,
};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ganymede", about = "Language-agnostic Jupyter kernel host")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Extra kernelspec roots, searched before the Jupyter data dirs.
    #[arg(long, global = true)]
    kernelspec_root: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List kernelspecs found in the Jupyter data directories.
    Kernelspec {
        #[command(subcommand)]
        cmd: KernelspecCmd,
    },
    /// Launch a kernel, run code, print outputs, shut it down.
    Run {
        /// Kernelspec name, or `fake` for the built-in in-process fake.
        #[arg(long, short)]
        kernel: String,
        /// Code to execute. Repeat for several cells.
        code: Vec<String>,
        /// Seconds to wait for the kernel to answer.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Interrupt each cell after this many milliseconds (for testing).
        #[arg(long)]
        interrupt_after_ms: Option<u64>,
    },
    /// Execute a notebook headlessly and write the result.
    Exec {
        notebook: PathBuf,
        /// Output path. Defaults to overwriting the input.
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Kernelspec name. Defaults to the notebook's `metadata.kernelspec.name`.
        #[arg(long, short)]
        kernel: Option<String>,
        /// Keep going after a cell errors.
        #[arg(long)]
        allow_errors: bool,
        /// Seconds to wait for each cell.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Run code on a kernel that is already running, by connection file.
    Attach {
        connection_file: PathBuf,
        code: Vec<String>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum KernelspecCmd {
    List,
    /// Print the search roots.
    Paths,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let specs = FsKernelspecs::discover(cli.kernelspec_root.clone());
    match cli.cmd {
        Cmd::Kernelspec {
            cmd: KernelspecCmd::Paths,
        } => {
            for r in specs.roots() {
                println!("{}", r.display());
            }
            println!(
                "runtime: {}",
                ganymede_kernel_host::kernelspec::jupyter_runtime_dir().display()
            );
        }
        Cmd::Kernelspec {
            cmd: KernelspecCmd::List,
        } => {
            for s in specs.list()? {
                println!(
                    "{:<16} {:<28} {:<12} {}",
                    s.name,
                    s.display_name,
                    s.language,
                    s.resource_dir.display()
                );
            }
        }
        Cmd::Run {
            kernel,
            code,
            timeout,
            interrupt_after_ms,
        } => {
            let timeout = Duration::from_secs(timeout);
            let interrupt_after = interrupt_after_ms.map(Duration::from_millis);
            if kernel == "fake" {
                let (host, side) = in_process_pair(64);
                ganymede_kernel_host::testing::FakeKernel::spawn(
                    side,
                    ganymede_kernel_host::testing::ReplyOrder::ReplyThenIdle,
                );
                let session = KernelSession::start(host, None);
                run_cells(&session, &code, timeout, interrupt_after).await?;
                return Ok(());
            }
            let spec = specs.get(&kernel)?;
            let launched = SubprocessProvisioner::default()
                .launch(&spec, LaunchOptions::default())
                .await?;
            let transport = ZmqTransport::connect(&launched.connection).await?;
            let session = KernelSession::start(transport, Some(&launched));
            let result = run_cells(&session, &code, timeout, interrupt_after).await;
            session.shutdown(false, Duration::from_secs(5)).await?;
            result?;
        }
        Cmd::Exec {
            notebook,
            out,
            kernel,
            allow_errors,
            timeout,
        } => {
            let mut nb = ganymede_nbformat::read_file(&notebook)
                .with_context(|| format!("reading {}", notebook.display()))?;
            let kernel = kernel
                .or_else(|| {
                    nb.metadata
                        .get("kernelspec")
                        .and_then(|k| k.get("name"))
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .context("no --kernel and no metadata.kernelspec.name in the notebook")?;
            let spec = specs.get(&kernel)?;
            let launched = SubprocessProvisioner::default()
                .launch(
                    &spec,
                    LaunchOptions {
                        cwd: notebook
                            .parent()
                            .map(|p| p.to_path_buf())
                            .filter(|p| !p.as_os_str().is_empty()),
                        ..Default::default()
                    },
                )
                .await?;
            let transport = ZmqTransport::connect(&launched.connection).await?;
            let session = KernelSession::start(transport, Some(&launched));
            let timeout = Duration::from_secs(timeout);
            session
                .nudge(timeout)
                .await
                .context("kernel did not start")?;
            let opts = ganymede_exec::Options {
                timeout,
                allow_errors,
                ..Default::default()
            };
            let result = ganymede_exec::execute_notebook(&session, &mut nb, &opts).await;
            session.shutdown(false, Duration::from_secs(5)).await?;
            let out = out.unwrap_or(notebook);
            ganymede_nbformat::write_file(&nb, &out)
                .with_context(|| format!("writing {}", out.display()))?;
            match result {
                Ok(r) => eprintln!(
                    "executed {} cells, {} errored, wrote {}",
                    r.executed,
                    r.errored.len(),
                    out.display()
                ),
                Err(e) => {
                    eprintln!("stopped: {e}; wrote {}", out.display());
                    std::process::exit(1);
                }
            }
        }
        Cmd::Attach {
            connection_file,
            code,
            timeout,
        } => {
            let info = read_connection_file(&connection_file).context("reading connection file")?;
            let launched = attach(info);
            let transport = ZmqTransport::connect(&launched.connection).await?;
            let session = KernelSession::start(transport, Some(&launched));
            run_cells(&session, &code, Duration::from_secs(timeout), None).await?;
        }
    }
    Ok(())
}

async fn run_cells(
    session: &KernelSession,
    code: &[String],
    timeout: Duration,
    interrupt_after: Option<Duration>,
) -> Result<()> {
    let info = session
        .nudge(timeout)
        .await
        .context("kernel did not answer kernel_info_request")?;
    eprintln!(
        "kernel: {} {} (protocol {}, language {})",
        info.implementation,
        info.implementation_version,
        info.protocol_version,
        info.language_info.name
    );
    if !info.extra.is_empty() {
        eprintln!(
            "extra kernel_info fields: {}",
            serde_json::to_string(&info.extra)?
        );
    }

    // Answer stdin requests from the terminal, and print iopub from other
    // clients' executions, which is what a multi-client host would see.
    let mut sub = session.subscribe();
    let s2 = session.clone();
    let stdin_task = tokio::spawn(async move {
        while let Ok((ch, m)) = sub.recv().await {
            if ch == Channel::Stdin && m.msg_type() == "input_request" {
                let req: content::InputRequest = m.content_as().unwrap_or(content::InputRequest {
                    prompt: String::new(),
                    password: false,
                    extra: Default::default(),
                });
                eprint!("{}", req.prompt);
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let _ = s2
                    .reply_input(&m, line.trim_end_matches(['\n', '\r']))
                    .await;
            }
        }
    });

    for cell in code {
        let interrupter = interrupt_after.map(|after| {
            let s = session.clone();
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                if let Err(e) = s.interrupt().await {
                    eprintln!("interrupt failed: {e}");
                }
            })
        });
        let exec = session
            .execute(ExecuteRequest::new(cell.clone()), timeout)
            .await?;
        if let Some(t) = interrupter {
            t.abort();
        }
        for out in &exec.outputs {
            print_output(out);
        }
        let reply: content::ExecuteReply = exec.reply.content_as()?;
        let count = reply
            .execution_count
            .map(|n| n.to_string())
            .unwrap_or_default();
        eprintln!("[{count}] {:?}", reply.status);
    }
    stdin_task.abort();
    Ok(())
}

fn print_output(m: &Message) {
    match m.msg_type() {
        "stream" => {
            if let Ok(s) = m.content_as::<content::Stream>() {
                print!("{}", s.text);
            }
        }
        "execute_result" | "display_data" => {
            if let Some(text) = m.content["data"].get("text/plain").and_then(|v| v.as_str()) {
                println!("{text}");
            } else {
                let keys: Vec<_> = m.content["data"]
                    .as_object()
                    .map(|o| o.keys().cloned().collect())
                    .unwrap_or_default();
                println!("<{}: {}>", m.msg_type(), keys.join(", "));
            }
        }
        "error" => {
            if let Ok(e) = m.content_as::<content::ErrorContent>() {
                for line in &e.traceback {
                    eprintln!("{line}");
                }
                if e.traceback.is_empty() {
                    eprintln!("{}: {}", e.ename, e.evalue);
                }
            }
        }
        "execute_input" | "clear_output" | "update_display_data" => {}
        other => eprintln!("<{other}>"),
    }
}
