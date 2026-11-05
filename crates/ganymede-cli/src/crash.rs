//! Opt-in crash reports to Sentry (#386, docs/privacy.md).
//!
//! Off unless the user turns it on with `GANYMEDE_CRASH_REPORTING` or
//! `crash_reporting` in `ganymede.toml`, and compiled in only with the
//! `crash-reporting` feature. When it is off nothing is initialized, so no
//! request is made. Reports carry the panic or error, its stack trace, the
//! version and the OS; everything else is dropped in `enabled::scrub` before a
//! report leaves the machine.

use std::ffi::OsStr;
use std::path::Path;

/// Overrides the config file: `1` (or `true`, `yes`, `on`) turns reporting
/// on, `0` (or `false`, `no`, `off`) turns it off.
pub const ENV: &str = "GANYMEDE_CRASH_REPORTING";

/// Whether the user opted in: [`ENV`] when it is set to a value it knows,
/// else `crash_reporting` in `config` (the config file's text, if any).
/// A config file that doesn't parse counts as not opted in; the command
/// that reads it reports the error.
pub fn opted_in(env: Option<&OsStr>, config: Option<&str>) -> bool {
    let from_env =
        env.and_then(OsStr::to_str)
            .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            });
    from_env.unwrap_or_else(|| {
        config
            .and_then(|text| ganymede_server::config::Config::from_toml(text).ok())
            .is_some_and(|c| c.crash_reporting)
    })
}

/// The config file `ganymede` reads: `--config` for the commands that
/// serve, else `ganymede.toml` in the Jupyter config dir when it exists.
pub fn config_text(explicit: Option<&Path>) -> Option<String> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => ganymede_engine::host::kernelspec::jupyter_config_dir()
            .join(ganymede_server::serve::DEFAULT_CONFIG_FILE),
    };
    std::fs::read_to_string(path).ok()
}

/// Replaces the home directory with `~` and keeps only the first line, at
/// most [`MAX_TEXT`] characters: a message is for telling crashes apart,
/// and its later lines are where data (a file's contents, a kernel's
/// output) would be.
#[cfg_attr(not(feature = "crash-reporting"), allow(dead_code))]
pub fn scrub_text(text: &str, home: Option<&str>) -> String {
    let line = text.lines().next().unwrap_or("");
    let line = match home {
        Some(h) if !h.is_empty() && h != "/" => line.replace(h, "~"),
        _ => line.to_owned(),
    };
    match line.char_indices().nth(MAX_TEXT) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line,
    }
}

/// The longest message or exception value sent, in characters.
#[cfg_attr(not(feature = "crash-reporting"), allow(dead_code))]
pub const MAX_TEXT: usize = 200;

#[cfg(feature = "crash-reporting")]
pub use enabled::*;

#[cfg(feature = "crash-reporting")]
mod enabled {
    use super::scrub_text;
    use sentry::protocol::{Event, Frame};
    use std::sync::Arc;
    use std::time::Duration;

    /// The `ganymede` project in Sentry (#384). A DSN only allows sending
    /// events, so it is safe in a binary.
    pub const DSN: &str = "https://62f5117396f2756828506b4cab813b6b@o4512380800598016.ingest.us.sentry.io/4512380975906816";

    /// Reporting, while this lives; dropping it flushes what is queued.
    pub struct Reporting {
        _guard: sentry::ClientInitGuard,
    }

    /// Starts reporting panics and errors to `dsn`. Returns the guard and
    /// the tracing layer that reports `error!` events from Ganymede's own
    /// crates.
    pub fn start(dsn: &str) -> (Reporting, ganymede_server::serve::ExtraLayer) {
        let home = std::env::home_dir().map(|h| h.to_string_lossy().into_owned());
        let version = env!("GANYMEDE_VERSION");
        let environment = if version.contains("+main") {
            "main-build"
        } else if cfg!(debug_assertions) {
            "development"
        } else {
            "release"
        };
        let mut options = sentry::ClientOptions::default();
        options.dsn = dsn.parse().ok();
        options.release = Some(format!("ganymede@{version}").into());
        options.environment = Some(environment.into());
        options.send_default_pii = false;
        options.attach_stacktrace = true;
        options.before_send = Some(Arc::new(move |event| Some(scrub(event, home.as_deref()))));
        options.before_breadcrumb = Some(Arc::new(|_| None));
        let agent = agent();
        options.transport = Some(Arc::new(move |opts: &sentry::ClientOptions| {
            let transport: Arc<dyn sentry::Transport> =
                match sentry::TransportOptions::try_from_client_options(opts) {
                    Some(general) => Arc::new(
                        sentry::transports::UreqHttpTransportOptions::from(general)
                            .with_agent(agent.clone())
                            .build(),
                    ),
                    None => Arc::new(Noop),
                };
            transport
        }));
        let guard = sentry::init(options);
        (Reporting { _guard: guard }, layer())
    }

    /// The HTTP client for reports. ureq's default has no timeouts, and
    /// sentry joins its sender thread on exit, so an unanswered request
    /// would hang `ganymede` as it quits. These bound that wait.
    fn agent() -> ureq::Agent {
        use ureq::tls::{RootCerts, TlsConfig};
        let config = ureq::Agent::config_builder()
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent(concat!("ganymede/", env!("CARGO_PKG_VERSION")))
            .build();
        ureq::Agent::new_with_config(config)
    }

    /// Stands in when the DSN is missing; sentry sends nothing then anyway.
    struct Noop;

    impl sentry::Transport for Noop {
        fn send_envelope(&self, _: sentry::Envelope) {}
    }

    /// `error!` events from Ganymede's crates become reports; nothing else
    /// (no other levels, no spans, no other crates' events) reaches Sentry.
    fn layer() -> ganymede_server::serve::ExtraLayer {
        use tracing_subscriber::Layer;
        Box::new(
            sentry::integrations::tracing::ErrorLayer::new().with_filter(
                tracing_subscriber::filter::filter_fn(|m| {
                    m.is_event()
                        && *m.level() == tracing::Level::ERROR
                        && m.target().starts_with("ganymede")
                }),
            ),
        )
    }

    /// What may leave the machine (docs/privacy.md): the error's type and
    /// first line, its stack frames' functions and paths (home rewritten
    /// to `~`), the release, and the OS, device and Rust contexts. Host
    /// name, user, request, breadcrumbs, extra data, tags, local
    /// variables and source lines are dropped.
    pub fn scrub(mut event: Event<'static>, home: Option<&str>) -> Event<'static> {
        event.server_name = None;
        event.user = None;
        event.request = None;
        event.breadcrumbs.values.clear();
        event.extra.clear();
        event.tags.clear();
        event
            .contexts
            .retain(|k, _| matches!(k.as_str(), "os" | "device" | "rust"));
        if let Some(sentry::protocol::Context::Device(d)) = event.contexts.get_mut("device") {
            d.name = None;
        }
        event.message = event.message.map(|m| scrub_text(&m, home));
        if let Some(entry) = &mut event.logentry {
            entry.message = scrub_text(&entry.message, home);
            entry.params.clear();
        }
        for e in &mut event.exception.values {
            e.value = e.value.as_deref().map(|v| scrub_text(v, home));
            for st in [&mut e.stacktrace, &mut e.raw_stacktrace]
                .into_iter()
                .flatten()
            {
                st.frames.iter_mut().for_each(|f| scrub_frame(f, home));
            }
        }
        for t in &mut event.threads.values {
            t.name = None;
            for st in [&mut t.stacktrace, &mut t.raw_stacktrace]
                .into_iter()
                .flatten()
            {
                st.frames.iter_mut().for_each(|f| scrub_frame(f, home));
            }
        }
        if let Some(st) = &mut event.stacktrace {
            st.frames.iter_mut().for_each(|f| scrub_frame(f, home));
        }
        event
    }

    fn scrub_frame(f: &mut Frame, home: Option<&str>) {
        f.abs_path = f.abs_path.as_deref().map(|p| scrub_text(p, home));
        f.filename = f.filename.as_deref().map(|p| scrub_text(p, home));
        f.vars.clear();
        f.pre_context.clear();
        f.context_line = None;
        f.post_context.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn env_wins_over_the_config_file() {
        let on = Some("crash_reporting = true\n");
        let off = Some("crash_reporting = false\n");
        let env = |v: &str| Some(OsString::from(v));
        assert!(!opted_in(None, None), "off by default");
        assert!(!opted_in(None, Some("port = 1\n")));
        assert!(opted_in(None, on));
        assert!(!opted_in(None, off));
        assert!(opted_in(env("1").as_deref(), off));
        assert!(opted_in(env(" Yes ").as_deref(), None));
        assert!(!opted_in(env("0").as_deref(), on));
        assert!(!opted_in(env("off").as_deref(), on));
        assert!(
            opted_in(env("maybe").as_deref(), on),
            "unknown values defer to the file"
        );
        assert!(
            !opted_in(None, Some("crash_reporting = \"yes\"\n")),
            "a bad file is off"
        );
    }

    #[test]
    fn text_loses_home_later_lines_and_length() {
        let home = Some("/Users/ada");
        assert_eq!(
            scrub_text("reading /Users/ada/notes/a.ipynb: denied", home),
            "reading ~/notes/a.ipynb: denied"
        );
        assert_eq!(scrub_text("first\nsecret cell output", home), "first");
        let long = "x".repeat(MAX_TEXT + 10);
        assert_eq!(scrub_text(&long, None).chars().count(), MAX_TEXT + 1);
        assert_eq!(
            scrub_text("/x", Some("/")),
            "/x",
            "a root home is left alone"
        );
    }

    #[cfg(feature = "crash-reporting")]
    #[test]
    fn events_keep_only_what_the_privacy_page_lists() {
        use sentry::protocol::{
            Breadcrumb, Context, DeviceContext, Event, Exception, Frame, Map, Stacktrace,
        };
        let mut event = Event {
            message: Some("at /home/u/nb.ipynb\ncell source".into()),
            server_name: Some("my-laptop".into()),
            ..Default::default()
        };
        event.breadcrumbs.values.push(Breadcrumb::default());
        event.extra.insert("body".into(), "secret".into());
        event.tags.insert("host".into(), "my-laptop".into());
        event.contexts.insert(
            "device".into(),
            Context::Device(Box::new(DeviceContext {
                name: Some("Ada's Mac".into()),
                arch: Some("arm64".into()),
                ..Default::default()
            })),
        );
        event
            .contexts
            .insert("Rust Tracing Fields".into(), Context::Other(Map::new()));
        let mut frame = Frame {
            abs_path: Some("/home/u/src/ganymede/main.rs".into()),
            context_line: Some("let secret = 1;".into()),
            ..Default::default()
        };
        frame.vars.insert("secret".into(), "1".into());
        event.exception.values.push(Exception {
            ty: "panic".into(),
            value: Some("bad /home/u/x\nmore".into()),
            stacktrace: Some(Stacktrace {
                frames: vec![frame],
                ..Default::default()
            }),
            ..Default::default()
        });

        let e = enabled::scrub(event, Some("/home/u"));
        assert_eq!(e.message.as_deref(), Some("at ~/nb.ipynb"));
        assert_eq!(e.server_name, None);
        assert!(e.breadcrumbs.values.is_empty());
        assert!(e.extra.is_empty());
        assert!(e.tags.is_empty());
        assert!(!e.contexts.contains_key("Rust Tracing Fields"));
        match &e.contexts["device"] {
            Context::Device(d) => {
                assert_eq!(d.name, None);
                assert_eq!(d.arch.as_deref(), Some("arm64"));
            }
            other => panic!("{other:?}"),
        }
        let ex = &e.exception.values[0];
        assert_eq!(ex.ty, "panic");
        assert_eq!(ex.value.as_deref(), Some("bad ~/x"));
        let f = &ex.stacktrace.as_ref().unwrap().frames[0];
        assert_eq!(f.abs_path.as_deref(), Some("~/src/ganymede/main.rs"));
        assert!(f.vars.is_empty());
        assert_eq!(f.context_line, None);
    }

    /// Off sends nothing; on sends to the DSN. A local listener stands in
    /// for Sentry.
    #[cfg(feature = "crash-reporting")]
    #[test]
    fn only_an_opted_in_start_reaches_sentry() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Duration;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dsn = format!("http://key@{}/1", listener.local_addr().unwrap());
        // Answer each request with 200 and report that it came.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                let _ = tx.send(());
            }
        });

        // Not opted in: main never calls start, so a captured error goes
        // nowhere.
        assert!(!opted_in(Some(OsStr::new("0")), None));
        sentry::capture_message("not sent", sentry::Level::Error);
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "nothing without start"
        );

        let (reporting, _layer) = start(&dsn);
        sentry::capture_message("sent", sentry::Level::Error);
        drop(reporting);
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "a report after start"
        );
    }
}
