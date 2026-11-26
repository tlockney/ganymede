//! Kernels on the gateway and this machine's own at once (#963): the
//! gateway's kernelspecs, then the local ones whose names these lack.
//!
//! With `gateway_url` set, the server runs kernels on another Jupyter
//! server. A machine that can also run them here keeps its own kernels
//! listed beside the remote ones, so a gateway that is asleep or off the
//! network doesn't leave an empty list with no reason: the API reports the
//! gateway's last refresh beside the specs.
//!
//! A gateway spec wins a name clash, so a notebook whose kernel the remote
//! offers still runs there, as the setting says. Launch and transport go to
//! whichever side lists the name, and a kernel connects back through the
//! side that launched it.

use async_trait::async_trait;
use ganymede_kernel_host::{
    Error, KernelProcess, KernelTransport, Kernelspec, KernelspecSource, LaunchOptions,
    LaunchedKernel, Provisioner, Result, TransportFactory,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A kernelspec source, provisioner and transport factory.
pub type Kernels = (
    Arc<dyn KernelspecSource>,
    Arc<dyn Provisioner>,
    Arc<dyn TransportFactory>,
);

/// Which side lists a kernelspec, and so runs its kernels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Gateway,
    Local,
}

/// The gateway's kernels and this machine's, as one source: list, launch and
/// connect each go to the side that has the kernel.
pub struct Combined {
    gateway: Kernels,
    local: Kernels,
    /// The side that launched a kernel, by the address of the process handle
    /// in its [`LaunchedKernel`], as [`RegisteredKernels`] tracks its own
    /// launches. An entry goes once the kernel is connected.
    ///
    /// [`RegisteredKernels`]: ganymede_kernel_host::registered::RegisteredKernels
    launched: Mutex<HashMap<usize, Side>>,
}

impl Combined {
    /// The gateway's kernels first, then `local`'s.
    pub fn new(gateway: Kernels, local: Kernels) -> Combined {
        Combined {
            gateway,
            local,
            launched: Mutex::new(HashMap::new()),
        }
    }

    /// The side that lists `name`, as launch routes it; the gateway's first,
    /// since it wins a clash.
    fn side_of(&self, name: &str) -> Result<Side> {
        let listed = |kernels: &Kernels| -> Result<bool> {
            Ok(kernels
                .0
                .list()?
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(name)))
        };
        if listed(&self.gateway)? {
            return Ok(Side::Gateway);
        }
        if listed(&self.local)? {
            return Ok(Side::Local);
        }
        Err(Error::NoSuchKernelspec(name.to_string()))
    }
}

impl KernelspecSource for Combined {
    fn list(&self) -> Result<Vec<Kernelspec>> {
        let mut specs = self.gateway.0.list()?;
        for spec in &mut specs {
            spec.gateway = true;
        }
        for mut spec in self.local.0.list()? {
            if specs
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&spec.name))
            {
                continue;
            }
            spec.gateway = false;
            specs.push(spec);
        }
        Ok(specs)
    }
}

#[async_trait]
impl Provisioner for Combined {
    async fn launch(&self, spec: &Kernelspec, opts: LaunchOptions) -> Result<LaunchedKernel> {
        let side = self.side_of(&spec.name)?;
        let kernels = match side {
            Side::Gateway => &self.gateway,
            Side::Local => &self.local,
        };
        let launched = kernels.1.launch(spec, opts).await?;
        self.launched
            .lock()
            .unwrap()
            .insert(handle_key(&launched.process), side);
        Ok(launched)
    }
}

#[async_trait]
impl TransportFactory for Combined {
    async fn connect(&self, launched: &LaunchedKernel) -> Result<KernelTransport> {
        let side = self
            .launched
            .lock()
            .unwrap()
            .remove(&handle_key(&launched.process));
        match side {
            Some(Side::Gateway) => self.gateway.2.connect(launched).await,
            Some(Side::Local) => self.local.2.connect(launched).await,
            None => Err(Error::Unsupported(
                "connect: not a kernel either side launched",
            )),
        }
    }
}

fn handle_key(process: &Arc<dyn KernelProcess>) -> usize {
    Arc::as_ptr(process) as *const () as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn spec(name: &str) -> Kernelspec {
        serde_json::from_value::<Kernelspec>(serde_json::json!({
            "argv": [],
            "display_name": name,
        }))
        .map(|mut s| {
            s.name = name.into();
            s
        })
        .unwrap()
    }

    struct Specs(Vec<&'static str>);

    impl KernelspecSource for Specs {
        fn list(&self) -> Result<Vec<Kernelspec>> {
            Ok(self.0.iter().map(|n| spec(n)).collect())
        }
    }

    /// Launches an in-process kernel, counting the launches.
    struct Launcher(AtomicUsize);

    #[async_trait]
    impl Provisioner for Launcher {
        async fn launch(&self, _: &Kernelspec, _: LaunchOptions) -> Result<LaunchedKernel> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ganymede_kernel_host::in_process())
        }
    }

    /// Refuses to connect, saying whose it is, so a test can tell that
    /// connect went to the side that launched the kernel.
    struct Connector(&'static str);

    #[async_trait]
    impl TransportFactory for Connector {
        async fn connect(&self, _: &LaunchedKernel) -> Result<KernelTransport> {
            Err(Error::Unsupported(self.0))
        }
    }

    fn combined(
        gateway: Vec<&'static str>,
        local: Vec<&'static str>,
    ) -> (Combined, Arc<Launcher>, Arc<Launcher>) {
        let gateway_launcher = Arc::new(Launcher(AtomicUsize::new(0)));
        let local_launcher = Arc::new(Launcher(AtomicUsize::new(0)));
        let both = Combined::new(
            (
                Arc::new(Specs(gateway)),
                gateway_launcher.clone(),
                Arc::new(Connector("gateway")),
            ),
            (
                Arc::new(Specs(local)),
                local_launcher.clone(),
                Arc::new(Connector("local")),
            ),
        );
        (both, gateway_launcher, local_launcher)
    }

    #[test]
    fn lists_the_gateways_specs_then_this_machines_others() {
        let (both, _, _) = combined(vec!["python3", "deno"], vec!["DENO", "cdp-node"]);
        let listed = both.list().unwrap();
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        // The gateway's first, in its order; a local name it has is a clash
        // it wins, whatever the case.
        assert_eq!(names, ["python3", "deno", "cdp-node"]);
        assert!(listed[0].gateway && listed[1].gateway);
        assert!(!listed[2].gateway);
    }

    #[tokio::test]
    async fn launch_and_connect_go_to_the_side_that_has_the_name() {
        let (both, gateway_launcher, local_launcher) = combined(vec!["deno"], vec!["cdp-node"]);

        let remote = both
            .launch(&spec("deno"), LaunchOptions::default())
            .await
            .unwrap();
        assert_eq!(gateway_launcher.0.load(Ordering::SeqCst), 1);
        assert_eq!(local_launcher.0.load(Ordering::SeqCst), 0);
        let e = both.connect(&remote).await.err().unwrap();
        assert!(e.to_string().contains("gateway"), "{e}");

        let here = both
            .launch(&spec("cdp-node"), LaunchOptions::default())
            .await
            .unwrap();
        assert_eq!(local_launcher.0.load(Ordering::SeqCst), 1);
        let e = both.connect(&here).await.err().unwrap();
        assert!(e.to_string().contains("local"), "{e}");

        // A launch connects back once.
        let e = both.connect(&here).await.err().unwrap();
        assert!(e.to_string().contains("either side"), "{e}");
    }

    #[tokio::test]
    async fn a_name_neither_side_lists_is_no_such_kernelspec() {
        let (both, _, _) = combined(vec!["deno"], vec![]);
        let e = both.launch(&spec("r"), LaunchOptions::default()).await;
        match e {
            Err(Error::NoSuchKernelspec(name)) => assert_eq!(name, "r"),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("launched a kernelspec neither side lists"),
        }
    }
}
