use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessStatus {
    NotRunning,
    Running,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PCloudConfig {
    pub username: String,
    pub mount_point: String,
    pub cache_size_gb: u64,
    pub log_path: String,
    pub log_level: String,
    pub save_password: bool,
    pub binary_path: String,
}

impl Default for PCloudConfig {
    fn default() -> Self {
        Self {
            username: String::new(),
            mount_point: String::new(),
            cache_size_gb: 5,
            log_path: String::new(),
            log_level: "INFO".to_string(),
            save_password: false,
            binary_path: String::new(),
        }
    }
}

pub struct ProcessManager {
    pcloudcc_path: String,
}

impl ProcessManager {
    pub fn new(pcloudcc_path: String) -> Self {
        Self { pcloudcc_path }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    static FAKE_CLIENT_LOCK: Mutex<()> = Mutex::new(());

    struct FakeClient {
        path: PathBuf,
        _lock: MutexGuard<'static, ()>,
    }

    impl FakeClient {
        fn new(script: &str) -> Self {
            let lock = FAKE_CLIENT_LOCK.lock().unwrap();
            let path = std::env::temp_dir().join(format!(
                "pcloudcc-gui-test-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self { path, _lock: lock }
        }
        fn manager(&self) -> ProcessManager {
            ProcessManager::new(self.path.to_string_lossy().into_owned())
        }
    }

    impl Drop for FakeClient {
        fn drop(&mut self) {
            std::fs::remove_file(&self.path).unwrap();
        }
    }

    fn config() -> PCloudConfig {
        PCloudConfig {
            username: "test@example.invalid".into(),
            ..PCloudConfig::default()
        }
    }

    #[test]
    fn start_reports_actual_daemon_status() {
        let client = FakeClient::new(
            r#"if [ "$1" = "-k" ]; then
                    read -r command
                    printf 'pcloud> %s\nStatus:   CONNECTING\nDownload: 0\nUpload: 0\n' "$command"
                else
                    echo "Daemon process created"
                fi"#,
        );
        let result = client
            .manager()
            .start_with_timeout(&config(), "", Duration::ZERO)
            .unwrap();
        assert!(result.contains("Status:   CONNECTING"));
        assert!(!result.contains("Daemon process created"));
    }

    #[test]
    fn start_does_not_report_success_when_daemon_is_unreachable() {
        let client = FakeClient::new(
            r#"if [ "$1" = "-k" ]; then
                    echo "Status failed: Unable to connect to UNIX socket" >&2
                else
                    echo "Daemon process created"
                fi"#,
        );
        let error = client
            .manager()
            .start_with_timeout(&config(), "", Duration::ZERO)
            .unwrap_err();
        assert!(error.contains("daemon did not respond"));
        assert!(error.contains("Unable to connect"));
        assert!(error.contains("~/.pcloud/debug.log"));
    }

    #[test]
    fn status_rejects_empty_command_response() {
        let client = FakeClient::new(r#"echo "pcloud> status""#);
        assert!(client
            .manager()
            .status()
            .unwrap_err()
            .contains("Unexpected response"));
    }

    #[test]
    fn start_surfaces_launcher_failure() {
        let client = FakeClient::new("echo 'invalid options' >&2; exit 1");
        let error = client
            .manager()
            .start_with_timeout(&config(), "", Duration::ZERO)
            .unwrap_err();
        assert!(error.contains("invalid options"));
    }

    #[test]
    fn auth_quotes_password_and_does_not_return_echoed_secret() {
        let client = FakeClient::new(
            r#"read -r command
               printf '%s\n' "$command"
               echo 'Auth sent.'
               read -r command"#,
        );
        let response = client
            .manager()
            .send_auth("spaces \"quotes\" \\ slash", false)
            .unwrap();
        assert_eq!(response, "Auth sent.");
    }

    #[test]
    fn auth_rejects_line_breaks_and_missing_confirmation() {
        let client = FakeClient::new(
            r#"read -r command
               echo 'Unexpected response'
               read -r command"#,
        );
        assert!(client.manager().send_auth("bad\nvalue", false).is_err());
        assert!(client.manager().send_auth("password", false).is_err());
    }

    #[test]
    fn tfa_requires_daemon_confirmation() {
        let client = FakeClient::new(
            r#"read -r command
               echo '2FA code sent.'
               read -r command"#,
        );
        assert_eq!(
            client.manager().send_tfa("123456").unwrap(),
            "2FA code sent."
        );
    }

    #[test]
    fn stop_checks_daemon_is_no_longer_responding() {
        let client = FakeClient::new(
            r#"read -r command
               if [ "$command" = "finalize" ]; then
                   echo 'Exiting ...'
               else
                   echo 'Status failed: Unable to connect to UNIX socket' >&2
               fi"#,
        );
        assert!(client.manager().stop(false).is_ok());
    }

    #[test]
    fn stop_requires_confirmation_for_pending_transfers() {
        let client = FakeClient::new(
            r#"read -r command
               if [ "$command" = "finalize" ]; then
                   printf 'Warning: 1 pending transfer(s) detected\nStopping daemon will break pending transfers. Continue? (y/n): '
                   read -r answer
                   if [ "$answer" = y ]; then
                       echo 'Exiting ...'
                   else
                       echo 'Finalize cancelled'
                   fi
               else
                   echo 'Status failed: Unable to connect to UNIX socket' >&2
               fi"#,
        );
        assert_eq!(
            client.manager().stop(false).unwrap_err(),
            "Stopping may interrupt pending transfers."
        );
        assert!(client.manager().stop(true).is_ok());
    }

    #[test]
    fn stop_reports_when_shutdown_cannot_be_verified() {
        let client = FakeClient::new(
            r#"read -r command
               if [ "$command" = "finalize" ]; then
                   echo 'Exiting ...'
               else
                   echo 'status failed' >&2
               fi"#,
        );
        assert!(client
            .manager()
            .stop(false)
            .unwrap_err()
            .contains("Could not verify daemon shutdown"));
    }
}

impl ProcessManager {
    pub fn set_binary_path(&mut self, path: String) {
        self.pcloudcc_path = path;
    }

    pub fn start(&self, config: &PCloudConfig, password: &str) -> Result<String, String> {
        self.start_with_timeout(config, password, Duration::from_secs(15))
    }

    fn start_with_timeout(
        &self,
        config: &PCloudConfig,
        password: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        if config.username.trim().is_empty() {
            return Err("Enter your pCloud account email first.".to_string());
        }
        if !Path::new(&self.pcloudcc_path).is_file() {
            return Err(format!(
                "pcloudcc binary not found at: {}",
                self.pcloudcc_path
            ));
        }
        if config.cache_size_gb == 0 || config.cache_size_gb > 1024 {
            return Err("Cache size must be between 1 and 1024 GB.".to_string());
        }

        let mut command = Command::new(&self.pcloudcc_path);
        command.arg("-u").arg(config.username.trim()).arg("-d");
        command
            .arg("--cache-size")
            .arg(config.cache_size_gb.to_string());

        if !config.mount_point.trim().is_empty() {
            command.arg("-m").arg(config.mount_point.trim());
        }
        if !config.log_path.trim().is_empty() {
            command.arg("--log-path").arg(config.log_path.trim());
        }
        if !config.log_level.trim().is_empty() {
            command.arg("--log-level").arg(&config.log_level);
        }
        if config.save_password {
            command.arg("-s");
        }
        if !password.is_empty() {
            command.env("PCLOUD_ACCOUNT_PASSWORD", password);
        }

        let output = command
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("Could not start pcloudcc: {error}"))?;

        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if message.is_empty() {
                format!("pcloudcc exited with {}.", output.status)
            } else {
                message
            });
        }

        let deadline = Instant::now() + timeout;
        loop {
            match self.status() {
                Ok(status) => return Ok(status),
                Err(error) if Instant::now() >= deadline => {
                    let log = if config.log_path.trim().is_empty() {
                        "~/.pcloud/debug.log"
                    } else {
                        config.log_path.trim()
                    };
                    return Err(format!(
                        "pcloudcc accepted the start request, but the daemon did not respond. \
                         It may still be starting or may have exited. Check {log} and refresh \
                         before trying again. Last status error: {error}"
                    ));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    pub fn send_command(&self, command: &str) -> Result<String, String> {
        if !Path::new(&self.pcloudcc_path).is_file() {
            return Err(format!(
                "pcloudcc binary not found at: {}",
                self.pcloudcc_path
            ));
        }

        let mut child = Command::new(&self.pcloudcc_path)
            .arg("-k")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not connect to pcloudcc: {error}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            writeln!(stdin, "{command}")
                .map_err(|error| format!("Could not send command: {error}"))?;
            writeln!(stdin, "quit")
                .map_err(|error| format!("Could not close command session: {error}"))?;
        }

        let output = child
            .wait_with_output()
            .map_err(|error| format!("Could not read pcloudcc response: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if !stderr.is_empty() {
            Err(stderr)
        } else if output.status.success() {
            Ok(stdout)
        } else {
            Err(if stderr.is_empty() {
                if stdout.is_empty() {
                    format!("pcloudcc command exited with {}.", output.status)
                } else {
                    stdout
                }
            } else {
                stderr
            })
        }
    }

    pub fn status(&self) -> Result<String, String> {
        let output = self.send_command("status")?;
        if !output
            .lines()
            .any(|line| line.trim_start().starts_with("Status:"))
        {
            return Err(format!("Unexpected response to status command: {output}"));
        }
        Ok(output)
    }

    pub fn list_sync_folders(&self) -> Result<String, String> {
        self.send_command("sync list")
    }

    pub fn pause_sync(&self) -> Result<String, String> {
        self.send_command("sync pause")
    }

    pub fn resume_sync(&self) -> Result<String, String> {
        self.send_command("sync resume")
    }

    pub fn start_crypto(&self, password: &str) -> Result<String, String> {
        self.send_secret("crypto start", password, "Crypto started.")
    }

    pub fn send_auth(&self, password: &str, save: bool) -> Result<String, String> {
        let (command, expected) = if save {
            ("authsave", "Auth sent and saved.")
        } else {
            ("auth", "Auth sent.")
        };
        self.send_secret(command, password, expected)
    }

    pub fn send_tfa(&self, code: &str) -> Result<String, String> {
        self.send_secret("tfa", code, "2FA code sent.")
    }

    fn send_secret(&self, command: &str, secret: &str, expected: &str) -> Result<String, String> {
        if secret.is_empty() || secret.chars().any(|c| c.is_control()) {
            return Err("The value must not be empty or contain control characters.".to_string());
        }
        let quoted = secret.replace('\\', "\\\\").replace('"', "\\\"");
        let output = self
            .send_command(&format!("{command} \"{quoted}\""))
            .map_err(|_| format!("Could not send {command} to the daemon; check that it is running and accepting commands."))?;
        if output.lines().any(|line| line.trim() == expected) {
            Ok(expected.to_string())
        } else {
            Err(format!("The daemon did not confirm the {command} request."))
        }
    }

    pub fn stop_crypto(&self) -> Result<String, String> {
        self.send_command("crypto stop")
    }

    pub fn stop(&self, confirm_pending: bool) -> Result<String, String> {
        if !Path::new(&self.pcloudcc_path).is_file() {
            return Err(format!(
                "pcloudcc binary not found at: {}",
                self.pcloudcc_path
            ));
        }

        let mut child = Command::new(&self.pcloudcc_path)
            .arg("-k")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not connect to pcloudcc: {error}"))?;
        let mut stdin = Some(
            child
                .stdin
                .take()
                .ok_or("Could not open pcloudcc command input.")?,
        );
        let mut stdout = child
            .stdout
            .take()
            .ok_or("Could not read pcloudcc output.")?;
        let stderr = child.stderr.take();
        let stderr_reader = std::thread::spawn(move || {
            let mut output = String::new();
            if let Some(mut stream) = stderr {
                stream.read_to_string(&mut output)?;
            }
            Ok::<_, std::io::Error>(output)
        });
        if let Some(input) = stdin.as_mut() {
            input
                .write_all(b"finalize\n")
                .and_then(|_| input.flush())
                .map_err(|error| format!("Could not send stop command: {error}"))?;
        }

        let mut output = Vec::new();
        let mut buffer = [0; 512];
        let mut answered = false;
        loop {
            let count = stdout
                .read(&mut buffer)
                .map_err(|error| format!("Could not read pcloudcc response: {error}"))?;
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
            let response = String::from_utf8_lossy(&output);
            if response.contains("Continue? (y/n): ") && !answered {
                if confirm_pending {
                    if let Some(input) = stdin.as_mut() {
                        input
                            .write_all(b"y\n")
                            .and_then(|_| input.flush())
                            .map_err(|error| format!("Could not confirm stop: {error}"))?;
                    }
                } else {
                    if let Some(input) = stdin.as_mut() {
                        input
                            .write_all(b"n\n")
                            .and_then(|_| input.flush())
                            .map_err(|error| format!("Could not cancel stop: {error}"))?;
                    }
                }
                drop(stdin.take());
                answered = true;
            }
        }
        drop(stdin);
        let status = child
            .wait()
            .map_err(|error| format!("Could not wait for pcloudcc: {error}"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| "pcloudcc error reader stopped unexpectedly.".to_string())?
            .map_err(|error| format!("Could not read pcloudcc errors: {error}"))?;
        let output = String::from_utf8_lossy(&output).trim().to_string();

        if !confirm_pending && answered {
            return Err("Stopping may interrupt pending transfers.".to_string());
        }
        if !stderr.trim().is_empty() {
            Err(stderr.trim().to_string())
        } else if !status.success() {
            Err(format!("pcloudcc command exited with {status}."))
        } else if output.contains("Exiting ...") {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match self.status() {
                    Err(error) if error.contains("Unable to connect to UNIX socket") => {
                        return Ok(output);
                    }
                    Err(error) => {
                        return Err(format!("Could not verify daemon shutdown: {error}"));
                    }
                    Ok(_) if Instant::now() >= deadline => {
                        return Err(
                            "The daemon is still responding after the stop request.".to_string()
                        );
                    }
                    Ok(_) => std::thread::sleep(Duration::from_millis(250)),
                }
            }
        } else if status.success() {
            Err("pcloudcc did not confirm that the daemon stopped.".to_string())
        } else {
            Err(format!("pcloudcc command exited with {status}."))
        }
    }
}
