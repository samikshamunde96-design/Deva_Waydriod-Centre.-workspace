//! Structured, validated workflows composed from the same primitives the
//! individual commands use. Each step reports its own outcome, so a partial
//! run is visible instead of collapsed into one success flag.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::android::{
    capture_screenshot, detect_adb_devices, get_foreground_app, get_selected_device, launch_app,
    set_selected_device, validate_package_name,
};
use crate::waydroid::{
    WaydroidPowerManager, connect_waydroid_adb, start_session, waydroid_session_running,
};

#[derive(Clone, Debug, Serialize)]
pub struct WorkflowStep {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkflowReport {
    pub id: String,
    pub title: String,
    pub success: bool,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Default)]
struct Run {
    steps: Vec<WorkflowStep>,
}

impl Run {
    fn step(&mut self, name: &str, result: Result<String, String>) -> bool {
        let ok = result.is_ok();
        self.steps.push(WorkflowStep {
            name: name.to_string(),
            ok,
            detail: result.unwrap_or_else(|error| error),
        });
        ok
    }

    fn finish(self, id: &str, title: &str) -> WorkflowReport {
        let success = !self.steps.is_empty() && self.steps.iter().all(|step| step.ok);
        WorkflowReport {
            id: id.to_string(),
            title: title.to_string(),
            success,
            steps: self.steps,
        }
    }
}

const BRING_UP: &str = "Bring up Waydroid and connect ADB";
const LAUNCH: &str = "Launch and verify an application";
const SCREENSHOT: &str = "Capture a screenshot to the host";

fn select_waydroid_device() -> Result<String, String> {
    let devices = detect_adb_devices()?;
    let device = devices
        .iter()
        .find(|device| device.is_waydroid && device.state == "device")
        .ok_or_else(|| "No ready Waydroid ADB device was discovered.".to_string())?;
    let selected = set_selected_device(&device.serial)?;
    Ok(format!(
        "Selected {} ({}).",
        if selected.model.is_empty() {
            "the Waydroid device"
        } else {
            selected.model.as_str()
        },
        selected.serial
    ))
}

fn ensure_ready() -> Result<String, String> {
    let device = get_selected_device()?;
    WaydroidPowerManager::new().ensure_android_ready(&device)
}

fn bring_up() -> WorkflowReport {
    let mut run = Run::default();
    let session_ok = if waydroid_session_running().unwrap_or(false) {
        run.step(
            "Check Waydroid session",
            Ok("Waydroid session is already running.".to_string()),
        )
    } else {
        run.step("Start Waydroid session", start_session())
    };
    if !session_ok {
        return run.finish("bring_up", BRING_UP);
    }
    if !run.step("Connect ADB", connect_waydroid_adb()) {
        return run.finish("bring_up", BRING_UP);
    }
    if !run.step("Select Waydroid device", select_waydroid_device()) {
        return run.finish("bring_up", BRING_UP);
    }
    run.step("Verify Android readiness", ensure_ready());
    run.finish("bring_up", BRING_UP)
}

fn launch(package: &str) -> WorkflowReport {
    let mut run = Run::default();
    if !run.step("Verify Android readiness", ensure_ready()) {
        return run.finish("launch", LAUNCH);
    }
    let device = match get_selected_device() {
        Ok(device) => device,
        Err(error) => {
            run.step("Read selected device", Err(error));
            return run.finish("launch", LAUNCH);
        }
    };
    if !run.step(
        &format!("Launch {package}"),
        launch_app(&device.serial, package),
    ) {
        return run.finish("launch", LAUNCH);
    }
    run.step(
        "Confirm foreground application",
        get_foreground_app(&device.serial),
    );
    run.finish("launch", LAUNCH)
}

fn save_screenshot(bytes: Vec<u8>) -> Result<String, String> {
    let directory = std::env::var("XDG_PICTURES_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| Path::new(&home).join("Pictures"))
        })
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Screenshot folder could not be created: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let path = directory.join(format!("waydroid-screenshot-{stamp}.png"));
    std::fs::write(&path, &bytes)
        .map_err(|error| format!("Screenshot could not be written: {error}"))?;
    Ok(format!(
        "Saved {} KiB to {}.",
        bytes.len() / 1024,
        path.display()
    ))
}

fn screenshot() -> WorkflowReport {
    let mut run = Run::default();
    if !run.step("Verify Android readiness", ensure_ready()) {
        return run.finish("screenshot", SCREENSHOT);
    }
    run.step(
        "Capture and save screenshot",
        capture_screenshot().and_then(save_screenshot),
    );
    run.finish("screenshot", SCREENSHOT)
}

pub fn run(id: &str, package: Option<&str>) -> Result<WorkflowReport, String> {
    match id {
        "bring_up" => Ok(bring_up()),
        "launch" => {
            let package = package
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "The launch workflow requires a package name.".to_string())?;
            validate_package_name(package)?;
            Ok(launch(package))
        }
        "screenshot" => Ok(screenshot()),
        other => Err(format!("Unknown workflow '{other}'.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_workflow_is_rejected() {
        assert!(run("reboot_everything", None).is_err());
    }

    #[test]
    fn launch_workflow_requires_a_valid_package() {
        assert!(run("launch", None).is_err());
        assert!(run("launch", Some("not a package")).is_err());
    }

    #[test]
    fn report_success_requires_every_step_to_pass() {
        let mut run = Run::default();
        run.step("first", Ok("done".to_string()));
        assert!(run.finish("id", "title").success);

        let mut run = Run::default();
        run.step("first", Ok("done".to_string()));
        run.step("second", Err("failed".to_string()));
        let report = run.finish("id", "title");
        assert!(!report.success);
        assert_eq!(report.steps.len(), 2);
    }
}
