//! Windows Service integration: register needle's daemon to run once as
//! LocalSystem and auto-start at boot, so the user never has to launch (or
//! elevate) the `serve` daemon manually. The MCP frontend just connects to the
//! always-running service over loopback.

use anyhow::{anyhow, Context, Result};
use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;
use windows_service::service::{
    ServiceAccess, ServiceErrorControl, ServiceExitCode, ServiceInfo, ServiceControl,
    ServiceControlAccept, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "needled";
const DISPLAY_NAME: &str = "needle file-search daemon";
const DESCRIPTION: &str =
    "Ultra-fast whole-machine NTFS filename search (MFT + USN) for AI agents.";

/// Register the service with the SCM and start it. Requires admin.
pub fn install() -> Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("failed to open Service Control Manager (run elevated)")?;

    let exe = std::env::current_exe().context("cannot resolve current exe path")?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        // SCM launches: needle.exe service run
        launch_arguments: vec![OsString::from("service"), OsString::from("run")],
        dependencies: vec![],
        account_name: None, // None = LocalSystem
        account_password: None,
    };

    let service = manager
        .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
        .context("failed to create service (is it already installed?)")?;
    let _ = service.set_description(DESCRIPTION);
    service
        .start::<OsString>(&[])
        .context("service created but failed to start")?;
    Ok(())
}

/// Stop (if running) and delete the service. Requires admin.
pub fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("failed to open Service Control Manager (run elevated)")?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )
        .context("service not found (already uninstalled?)")?;

    // Best-effort stop before delete.
    if let Ok(status) = service.query_status() {
        if status.current_state != ServiceState::Stopped {
            let _ = service.stop();
            // Give it a moment to wind down.
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(s) = service.query_status() {
                    if s.current_state == ServiceState::Stopped {
                        break;
                    }
                }
            }
        }
    }
    service.delete().context("failed to delete service")?;
    Ok(())
}

/// Entry point used by `needle service run` — hands control to the SCM dispatcher.
pub fn run_dispatch() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| anyhow!("service dispatcher failed (only the SCM should call `service run`): {e}"))
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_args: Vec<OsString>) {
    // Errors here can't propagate; the service simply won't reach Running.
    let _ = run_service();
}

fn run_service() -> windows_service::Result<()> {
    let (shutdown_tx, shutdown_rx) = mpsc::channel();

    let event_handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = shutdown_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

    let running = ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    status_handle.set_service_status(running.clone())?;

    // Run the actual daemon (index + USN refresh + loopback query server) on a
    // background thread; it blocks on accept() for the service's lifetime.
    std::thread::spawn(|| {
        let _ = crate::run_serve(crate::ipc::DEFAULT_ADDR);
    });

    // Block until the SCM asks us to stop.
    let _ = shutdown_rx.recv();

    status_handle.set_service_status(ServiceStatus {
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        ..running
    })?;
    Ok(())
}
