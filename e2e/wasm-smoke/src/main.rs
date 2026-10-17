//! A notebook run through the engine on an in-process kernel, with no
//! sockets, subprocesses, or threads: what an embedded host (an iPad app, a
//! browser worker) has. Built for `wasm32-wasip1` and run under Node's WASI
//! by `e2e/wasm-smoke/run.sh`; prints the executed notebook.

use ganymede_engine::host::testing::{FakeKernel, ReplyOrder};
use ganymede_engine::host::in_process_pair;
use ganymede_engine::{Engine, Notebook, NotebookOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (host, side) = in_process_pair(64);
    FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
    let kernel = Engine::new().adopt(host).await.expect("kernel ready");
    let r = kernel.execute("hello").await.expect("execute");
    assert_eq!(r.text(), "echo: hello\nHELLO\n");

    let mut nb = Notebook::from_str(
        r#"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[
          {"cell_type":"code","id":"a","metadata":{},"source":"one","outputs":[],"execution_count":null},
          {"cell_type":"code","id":"b","metadata":{},"source":"two","outputs":[],"execution_count":null}]}"#,
    )
    .unwrap();
    let report = kernel
        .run_notebook(&mut nb, &NotebookOptions::default())
        .await
        .expect("notebook runs");
    assert_eq!(report.executed, 2);
    print!("{}", nb.to_file_string());
    kernel.shutdown().await.unwrap();
}
