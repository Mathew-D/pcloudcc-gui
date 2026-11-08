mod open_folder;
mod process_manager;
mod tray;

use eframe::egui;
use process_manager::{PCloudConfig, ProcessManager, ProcessStatus};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Sync,
    Crypto,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Status,
    Folders,
    Start,
    Stop,
    Pause,
    Resume,
    CryptoStart,
    CryptoStop,
    Auth,
    Tfa,
}

enum Operation {
    SetBinaryPath(String),
    Status,
    Folders,
    Start(PCloudConfig, String),
    Stop(bool),
    Pause,
    Resume,
    CryptoStart(String),
    CryptoStop,
    Auth(String, bool),
    Tfa(String),
}

struct OperationResult {
    action: ActionKind,
    result: Result<String, String>,
}

struct PCloudGui {
    page: Page,
    config: PCloudConfig,
    account_password: String,
    crypto_password: String,
    tfa_code: String,
    tfa_armed: bool,
    show_account_password: bool,
    show_crypto_password: bool,
    binary_path: String,
    config_path: Option<PathBuf>,
    status: ProcessStatus,
    status_checked: bool,
    status_output: String,
    folders_output: String,
    busy: Option<ActionKind>,
    confirm_stop: bool,
    close_requested: bool,
    close_ready: bool,
    notice: Option<(bool, String)>,
    request_tx: Sender<Operation>,
    result_rx: Receiver<OperationResult>,
    folder_tx: Sender<Result<(), String>>,
    folder_rx: Receiver<Result<(), String>>,
    last_status_refresh: Instant,
    tray: Option<tray::Tray>,
    hidden_to_tray: bool,
    awaiting_unminimize: bool,
}

impl PCloudGui {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        set_theme(&cc.egui_ctx);
        let (request_tx, request_rx) = mpsc::channel::<Operation>();
        let (result_tx, result_rx) = mpsc::channel::<OperationResult>();
        let (folder_tx, folder_rx) = mpsc::channel();
        let config_path = config_file_path();
        let (config, mut load_notice) = load_config(config_path.as_deref());
        let tray = match tray::Tray::new(cc.egui_ctx.clone()) {
            Ok(tray) => Some(tray),
            Err(error) => {
                load_notice = Some(match load_notice {
                    Some(existing) => format!("{existing} {error}"),
                    None => error,
                });
                None
            }
        };
        let binary_path = if config.binary_path.trim().is_empty() {
            find_pcloudcc_binary()
        } else {
            if !std::path::Path::new(&config.binary_path).is_file() {
                let message = format!(
                    "Saved executable not found: {}. In Settings, choose Find automatically or select the correct path.",
                    config.binary_path
                );
                load_notice = Some(match load_notice {
                    Some(existing) => format!("{existing} {message}"),
                    None => message,
                });
            }
            config.binary_path.clone()
        };
        let ui_binary_path = binary_path.clone();

        std::thread::spawn(move || {
            let mut manager = ProcessManager::new(binary_path.clone());
            while let Ok(operation) = request_rx.recv() {
                let operation = match operation {
                    Operation::SetBinaryPath(path) => {
                        manager.set_binary_path(path);
                        continue;
                    }
                    operation => operation,
                };
                let (action, result) = execute_operation(&mut manager, operation);
                if result_tx.send(OperationResult { action, result }).is_err() {
                    break;
                }
            }
        });

        let mut app = Self {
            page: Page::Overview,
            config,
            account_password: String::new(),
            crypto_password: String::new(),
            tfa_code: String::new(),
            tfa_armed: false,
            show_account_password: false,
            show_crypto_password: false,
            binary_path: ui_binary_path,
            config_path,
            status: ProcessStatus::NotRunning,
            status_checked: false,
            status_output: String::new(),
            folders_output: String::new(),
            busy: None,
            confirm_stop: false,
            close_requested: false,
            close_ready: false,
            notice: load_notice.map(|notice| (false, notice)),
            request_tx,
            result_rx,
            folder_tx,
            folder_rx,
            last_status_refresh: Instant::now(),
            tray,
            hidden_to_tray: false,
            awaiting_unminimize: false,
        };
        app.queue(Operation::Status);
        app
    }

    fn queue(&mut self, operation: Operation) {
        let action = match &operation {
            Operation::SetBinaryPath(_) => return,
            Operation::Status => ActionKind::Status,
            Operation::Folders => ActionKind::Folders,
            Operation::Start(_, _) => ActionKind::Start,
            Operation::Stop(_) => ActionKind::Stop,
            Operation::Pause => ActionKind::Pause,
            Operation::Resume => ActionKind::Resume,
            Operation::CryptoStart(_) => ActionKind::CryptoStart,
            Operation::CryptoStop => ActionKind::CryptoStop,
            Operation::Auth(_, _) => ActionKind::Auth,
            Operation::Tfa(_) => ActionKind::Tfa,
        };
        match self.request_tx.send(operation) {
            Ok(()) => self.busy = Some(action),
            Err(error) => {
                self.close_requested = false;
                self.notice = Some((false, format!("Could not queue action: {error}")));
            }
        }
    }

    fn poll_results(&mut self) {
        loop {
            match self.result_rx.try_recv() {
                Ok(response) => {
                    self.busy = None;
                    match response.action {
                        ActionKind::Status => {
                            self.status_checked = true;
                            self.last_status_refresh = Instant::now();
                            match response.result {
                                Ok(output) => {
                                    self.status = ProcessStatus::Running;
                                    self.status_output = output;
                                }
                                Err(error) => {
                                    self.status = ProcessStatus::NotRunning;
                                    self.tfa_armed = false;
                                    self.status_output = format!(
                                        "Could not reach the daemon: {error}\n\
                                         Check the executable path in Settings and the pcloudcc log if it should be running."
                                    );
                                }
                            }
                        }
                        ActionKind::Folders => match response.result {
                            Ok(output) => {
                                self.folders_output = output;
                                self.notice = Some((true, "Sync folders refreshed.".to_string()));
                            }
                            Err(error) => self.notice = Some((false, error)),
                        },
                        ActionKind::Start => match response.result {
                            Ok(output) => {
                                self.status = ProcessStatus::Running;
                                self.status_checked = true;
                                self.status_output = output;
                                self.account_password.clear();
                                self.last_status_refresh = Instant::now();
                                self.notice = Some((
                                    true,
                                    "Daemon is responding. See its status below.".to_string(),
                                ));
                            }
                            Err(error) => {
                                self.tfa_armed = false;
                                self.status = ProcessStatus::Error(error.clone());
                                self.notice = Some((false, error));
                            }
                        },
                        ActionKind::Stop => match response.result {
                            Ok(output) => {
                                self.status = ProcessStatus::NotRunning;
                                self.status_output.clear();
                                self.tfa_armed = false;
                                self.tfa_code.clear();
                                self.notice = Some((
                                    true,
                                    nonempty_message(output, "pCloud daemon stopped."),
                                ));
                                if self.close_requested {
                                    self.close_ready = true;
                                }
                            }
                            Err(error) if error == "Stopping may interrupt pending transfers." => {
                                self.confirm_stop = true;
                            }
                            Err(error) => {
                                self.close_requested = false;
                                self.notice = Some((false, error));
                            }
                        },
                        ActionKind::Auth | ActionKind::Tfa => match response.result {
                            Ok(_) => {
                                if response.action == ActionKind::Auth {
                                    self.account_password.clear();
                                } else {
                                    self.tfa_code.clear();
                                }
                                self.notice = Some((
                                    true,
                                    "Request sent; waiting for authentication status.".to_string(),
                                ));
                                self.last_status_refresh = Instant::now() - Duration::from_secs(2);
                            }
                            Err(error) => {
                                if response.action == ActionKind::Auth {
                                    self.tfa_armed = false;
                                }
                                self.notice = Some((false, error));
                            }
                        },
                        ActionKind::Pause
                        | ActionKind::Resume
                        | ActionKind::CryptoStart
                        | ActionKind::CryptoStop => match response.result {
                            Ok(output) => {
                                if response.action == ActionKind::CryptoStart {
                                    self.crypto_password.clear();
                                }
                                let fallback = match response.action {
                                    ActionKind::Pause => "Sync paused.",
                                    ActionKind::Resume => "Sync resumed.",
                                    ActionKind::CryptoStart => "Encrypted folder unlocked.",
                                    ActionKind::CryptoStop => "Encrypted folder locked.",
                                    _ => unreachable!(),
                                };
                                self.notice = Some((true, nonempty_message(output, fallback)));
                            }
                            Err(error) => self.notice = Some((false, error)),
                        },
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.busy = None;
                    self.close_requested = false;
                    self.status = ProcessStatus::Error(
                        "The background worker stopped unexpectedly.".to_string(),
                    );
                    self.status_checked = false;
                    self.notice = Some((
                        false,
                        "The background worker stopped unexpectedly.".to_string(),
                    ));
                    break;
                }
            }
        }
        if let Some(tray) = &self.tray {
            tray.set_connected(tray_connected(&self.status, &self.status_output));
        }
    }

    fn save_config(&mut self) {
        self.config.binary_path = self.binary_path.trim().to_string();
        let Some(path) = &self.config_path else {
            self.notice = Some((
                false,
                "Could not determine a configuration directory.".to_string(),
            ));
            return;
        };
        let result = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let data = serde_json::to_vec_pretty(&self.config)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
            std::fs::write(path, data)
        })();
        self.notice = Some(match result {
            Ok(()) => (true, "Settings saved on this device.".to_string()),
            Err(error) => (false, format!("Could not save settings: {error}")),
        });
    }

    fn refresh_status(&mut self) {
        if self.busy.is_none() {
            self.queue(Operation::Status);
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) -> bool {
        let tray_available = self.tray.as_ref().is_some_and(tray::Tray::available);
        self.handle_close_with_tray(ctx, tray_available)
    }

    fn handle_close_with_tray(&mut self, ctx: &egui::Context, tray_available: bool) -> bool {
        if self.close_ready {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return true;
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.hidden_to_tray {
                return true;
            }
            if !self.close_requested && tray_available {
                self.hidden_to_tray = true;
                return true;
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.close_requested {
                self.notice = Some((
                    false,
                    "System tray unavailable. Use Exit to stop pCloudCC and close the GUI."
                        .to_string(),
                ));
            }
        }
        if self.close_requested && self.busy.is_none() && !self.confirm_stop {
            if matches!(self.status, ProcessStatus::NotRunning) && self.status_checked {
                self.close_ready = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return true;
            }
            self.queue(Operation::Stop(false));
        }
        false
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        self.close_requested = true;
        ctx.request_repaint();
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("p").size(23.0).strong().color(ACCENT));
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("pCloudCC").size(18.0).strong());
                ui.label(
                    egui::RichText::new("Desktop client")
                        .size(11.0)
                        .color(MUTED),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (label, color) = if self.busy.is_some() {
                    ("Working", WARNING)
                } else if !self.status_checked {
                    ("Checking", MUTED)
                } else {
                    match &self.status {
                        ProcessStatus::Running => {
                            let (label, _, color) =
                                status_description(daemon_status(&self.status_output));
                            (label, color)
                        }
                        ProcessStatus::NotRunning => ("Disconnected", MUTED),
                        ProcessStatus::Error(_) => ("Needs attention", ERROR),
                    }
                };
                ui.label(
                    egui::RichText::new(format!("● {label}"))
                        .color(color)
                        .strong(),
                );
                ui.add_space(12.0);
                if ui.button("Refresh").clicked() {
                    self.refresh_status();
                }
                if ui
                    .add_enabled(
                        self.tray.as_ref().is_some_and(tray::Tray::available),
                        egui::Button::new("Minimize to tray"),
                    )
                    .on_disabled_hover_text("A system tray host is required.")
                    .clicked()
                {
                    self.minimize_to_tray(ui.ctx());
                }
                if ui
                    .add_enabled(!self.close_requested, egui::Button::new("Exit"))
                    .on_hover_text("Stop pCloudCC and exit")
                    .clicked()
                {
                    self.request_exit(ui.ctx());
                }
            });
        });
    }

    fn minimize_to_tray(&mut self, ctx: &egui::Context) {
        if self.tray.as_ref().is_some_and(tray::Tray::available) && !self.close_requested {
            self.hide_window_for_tray(ctx);
        }
    }

    fn hide_window_for_tray(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ctx.request_repaint();
    }

    fn restore_window(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        self.awaiting_unminimize = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn handle_minimized(&mut self, ctx: &egui::Context, minimized: Option<bool>) {
        if minimized == Some(false) {
            self.awaiting_unminimize = false;
        } else if minimized == Some(true)
            && !self.awaiting_unminimize
            && !self.hidden_to_tray
            && !self.close_requested
        {
            self.minimize_to_tray(ctx);
        }
    }

    fn handle_tray_events(&mut self, ctx: &egui::Context) {
        while let Some(action) = self
            .tray
            .as_ref()
            .and_then(|tray| tray.events.try_recv().ok())
        {
            self.handle_tray_action(ctx, action);
        }
    }

    fn handle_tray_action(&mut self, ctx: &egui::Context, action: tray::TrayAction) {
        match action {
            tray::TrayAction::Show => self.restore_window(ctx),
            tray::TrayAction::OpenFolder => self.open_folder(Some(ctx.clone())),
            tray::TrayAction::Quit => {
                self.restore_window(ctx);
                self.request_exit(ctx);
            }
            tray::TrayAction::Offline => {
                self.restore_window(ctx);
                self.notice = Some((
                    false,
                    "System tray disconnected; the window has been restored.".to_string(),
                ));
            }
            tray::TrayAction::Online => {}
        }
    }

    fn open_folder(&self, context: Option<egui::Context>) {
        let configured = self.config.mount_point.clone();
        let sender = self.folder_tx.clone();
        std::thread::spawn(move || {
            let result = open_folder::open(&configured);
            if sender.send(result).is_ok() {
                if let Some(context) = context {
                    context.request_repaint();
                }
            }
        });
    }

    fn poll_folder_results(&mut self) {
        while let Ok(result) = self.folder_rx.try_recv() {
            if let Err(error) = result {
                self.notice = Some((false, error));
                self.hidden_to_tray = false;
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("navigation")
            .exact_size(178.0)
            .resizable(false)
            .frame(egui::Frame::side_top_panel(ui.style()).fill(PANEL))
            .show(ui, |ui| {
                ui.add_space(17.0);
                ui.label(
                    egui::RichText::new("WORKSPACE")
                        .size(10.0)
                        .strong()
                        .color(MUTED),
                );
                ui.add_space(8.0);
                self.nav_button(ui, Page::Overview, "◉", "Overview");
                self.nav_button(ui, Page::Sync, "↔", "Sync");
                self.nav_button(ui, Page::Crypto, "◆", "Crypto");
                ui.add_space(18.0);
                ui.label(
                    egui::RichText::new("PREFERENCES")
                        .size(10.0)
                        .strong()
                        .color(MUTED),
                );
                ui.add_space(8.0);
                self.nav_button(ui, Page::Settings, "⚙", "Settings");

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("Runs locally on your device")
                            .size(10.0)
                            .color(MUTED),
                    );
                    ui.label(egui::RichText::new("pcloudcc GUI").size(11.0).strong());
                    ui.add_space(12.0);
                });
            });
    }

    fn nav_button(&mut self, ui: &mut egui::Ui, page: Page, icon: &str, title: &str) {
        let selected = self.page == page;
        let response = ui.add_sized(
            [ui.available_width(), 37.0],
            egui::Button::new(
                egui::RichText::new(format!("{icon}    {title}")).color(if selected {
                    TEXT
                } else {
                    MUTED
                }),
            )
            .fill(if selected {
                SELECTED
            } else {
                egui::Color32::TRANSPARENT
            })
            .stroke(egui::Stroke::NONE),
        );
        if response.clicked() {
            self.page = page;
        }
    }

    fn overview_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Overview");
        ui.label(egui::RichText::new("Manage your pCloud connection and files.").color(MUTED));
        ui.add_space(18.0);

        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(18)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("pCloud connection").size(17.0).strong());
                        ui.add_space(3.0);
                        ui.label(egui::RichText::new(&self.config.username).color(MUTED));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match &self.status {
                            ProcessStatus::Running => {
                                let (label, _, color) = status_description(daemon_status(&self.status_output));
                                ui.label(egui::RichText::new(format!("● {label}")).color(color).strong())
                            },
                            ProcessStatus::Error(_) => ui.label(egui::RichText::new("● Error").color(ERROR).strong()),
                            ProcessStatus::NotRunning if self.status_checked => ui.label(egui::RichText::new("● Stopped").color(MUTED).strong()),
                            _ => ui.label(egui::RichText::new("● Checking…").color(WARNING).strong()),
                        };
                    });
                });
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let can_start = self.busy.is_none()
                        && !matches!(self.status, ProcessStatus::Running);
                    let can_stop = self.busy.is_none()
                        && matches!(self.status, ProcessStatus::Running);
                    if ui.add_enabled(can_start, primary_button("Start pCloudCC")).clicked() {
                        self.tfa_armed = !self.tfa_code.is_empty();
                        self.queue(Operation::Start(self.config.clone(), self.account_password.clone()));
                    }
                    if ui.add_enabled(can_stop, egui::Button::new("Stop").min_size([82.0, 34.0].into())).clicked() {
                        self.queue(Operation::Stop(false));
                    }
                    if self.busy.is_some() {
                        ui.spinner();
                    }
                });
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Sign in").strong());
                ui.label(egui::RichText::new(
                    "Enter your password and, if needed, a 2FA code. A code entered before starting or sending a password is sent only if pCloud requests it."
                ).size(11.0).color(MUTED));
                let running = matches!(self.status, ProcessStatus::Running);
                let auth_required = running && matches!(
                    daemon_status(&self.status_output),
                    Some("LOGIN_REQUIRED" | "BAD_LOGIN_DATA" | "BAD_LOGIN_TOKEN")
                );
                ui.horizontal(|ui| {
                    ui.add_sized([75.0, 25.0], egui::Label::new("Password"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.account_password)
                            .password(!self.show_account_password)
                            .desired_width(180.0),
                    );
                    ui.checkbox(&mut self.show_account_password, "Show");
                    if ui.add_enabled(
                        self.busy.is_none() && auth_required && !self.account_password.is_empty(),
                        egui::Button::new("Send password"),
                    ).on_disabled_hover_text("Available when the daemon requests a password.").clicked() {
                        self.tfa_armed = !self.tfa_code.is_empty();
                        self.queue(Operation::Auth(self.account_password.clone(), self.config.save_password));
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_sized([75.0, 25.0], egui::Label::new("2FA code"));
                    let code_response = ui.add(
                        egui::TextEdit::singleline(&mut self.tfa_code)
                            .password(true)
                            .desired_width(180.0)
                    );
                    if code_response.changed() && self.tfa_code.is_empty() {
                        self.tfa_armed = false;
                    }
                    if code_response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        && self.busy.is_none()
                        && running
                        && matches!(daemon_status(&self.status_output), Some("TFA_REQUIRED" | "BAD_TFA_CODE"))
                        && !self.tfa_code.is_empty()
                    {
                        self.queue(Operation::Tfa(self.tfa_code.clone()));
                    }
                });
                if self.tfa_armed {
                    ui.label(egui::RichText::new("2FA code will be sent if pCloud requests it.").size(11.0).color(WARNING));
                } else if running && matches!(daemon_status(&self.status_output), Some("TFA_REQUIRED" | "BAD_TFA_CODE")) {
                    ui.label(egui::RichText::new("Enter a new code and press Enter.").size(11.0).color(WARNING));
                }
            });

        ui.add_space(12.0);
        ui.columns(2, |columns| {
            metric_card(
                &mut columns[0],
                "CONFIGURED MOUNT POINT",
                display_or(&self.config.mount_point, "Default location"),
            );
            metric_card(
                &mut columns[1],
                "CACHE LIMIT",
                format!("{} GB", self.config.cache_size_gb),
            );
        });

        ui.add_space(12.0);
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(16)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Daemon status").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(self.busy.is_none(), egui::Button::new("Refresh"))
                            .clicked()
                        {
                            self.refresh_status();
                        }
                    });
                });
                ui.add_space(8.0);
                if matches!(self.status, ProcessStatus::Running) {
                    let (label, detail, color) =
                        status_description(daemon_status(&self.status_output));
                    ui.label(egui::RichText::new(label).size(16.0).strong().color(color));
                    ui.label(egui::RichText::new(detail).color(MUTED));
                    for field in ["Download:", "Upload:"] {
                        if let Some(value) = status_field(&self.status_output, field) {
                            ui.label(format!("{field} {value}"));
                        }
                    }
                    egui::CollapsingHeader::new("Technical details").show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&self.status_output)
                                    .monospace()
                                    .size(12.0),
                            )
                            .wrap(),
                        );
                    });
                } else if self.status_output.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No status details yet. Start pCloudCC or refresh the connection.",
                        )
                        .color(MUTED),
                    );
                } else {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&self.status_output)
                                .monospace()
                                .size(12.0),
                        )
                        .wrap(),
                    );
                }
            });

        if let ProcessStatus::Error(error) = &self.status {
            ui.add_space(8.0);
            ui.colored_label(ERROR, error);
        }
    }

    fn sync_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sync folders");
        ui.label(
            egui::RichText::new(
                "View folders configured for synchronization and control sync activity.",
            )
            .color(MUTED),
        );
        ui.add_space(18.0);
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(18)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.busy.is_none(), primary_button("Refresh folders"))
                        .clicked()
                    {
                        self.queue(Operation::Folders);
                    }
                    if ui
                        .add_enabled(self.busy.is_none(), egui::Button::new("Pause sync"))
                        .clicked()
                    {
                        self.queue(Operation::Pause);
                    }
                    if ui
                        .add_enabled(self.busy.is_none(), egui::Button::new("Resume sync"))
                        .clicked()
                    {
                        self.queue(Operation::Resume);
                    }
                });
                ui.add_space(14.0);
                if self.folders_output.is_empty() {
                    empty_state(
                        ui,
                        "No folder list loaded",
                        "Refresh to retrieve the sync folders configured in pcloudcc.",
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(380.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&self.folders_output).monospace(),
                                )
                                .wrap(),
                            );
                        });
                }
            });
    }

    fn crypto_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Crypto folder");
        ui.label(egui::RichText::new("Unlock or lock your encrypted pCloud folder.").color(MUTED));
        ui.add_space(18.0);
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(18)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Encrypted folder access").size(16.0).strong());
                ui.add_space(5.0);
                ui.label(egui::RichText::new("Your crypto password is only sent to pcloudcc and is never saved in GUI settings.").color(MUTED));
                ui.add_space(15.0);
                ui.horizontal(|ui| {
                    ui.label("Crypto password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.crypto_password)
                            .password(!self.show_crypto_password)
                            .desired_width(280.0),
                    );
                    ui.checkbox(&mut self.show_crypto_password, "Show");
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(
                        self.busy.is_none() && !self.crypto_password.is_empty(),
                        primary_button("Unlock folder"),
                    ).clicked() {
                        self.queue(Operation::CryptoStart(self.crypto_password.clone()));
                    }
                    if ui.add_enabled(self.busy.is_none(), egui::Button::new("Lock folder")).clicked() {
                        self.queue(Operation::CryptoStop);
                    }
                });
            });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.label(egui::RichText::new("Configure how pcloudcc starts on this device.").color(MUTED));
        ui.add_space(18.0);
        egui::Frame::group(ui.style())
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(18)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Account").size(15.0).strong());
                ui.add_space(8.0);
                form_row(ui, "Email address", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.config.username).desired_width(330.0));
                });
                form_row(ui, "Mount point", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.config.mount_point).desired_width(330.0).hint_text("Leave blank for default"));
                });
                form_row(ui, "Cache limit", |ui| {
                    ui.add(egui::Slider::new(&mut self.config.cache_size_gb, 1..=1024).suffix(" GB"));
                });
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Diagnostics").size(15.0).strong());
                ui.add_space(8.0);
                form_row(ui, "Log file", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.config.log_path).desired_width(330.0).hint_text("Default log location"));
                });
                form_row(ui, "Log level", |ui| {
                    egui::ComboBox::from_id_salt("log_level")
                        .selected_text(&self.config.log_level)
                        .show_ui(ui, |ui| {
                            for level in ["NONE", "ERROR", "WARNING", "INFO", "NOTICE", "DEBUG"] {
                                ui.selectable_value(&mut self.config.log_level, level.to_string(), level);
                            }
                        });
                });
                ui.add_space(4.0);
                ui.checkbox(&mut self.config.save_password, "Ask pcloudcc to save the account password");
                ui.label(egui::RichText::new("Only enable this on a trusted device. The GUI itself never stores the password.").size(11.0).color(MUTED));
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(egui::RichText::new("pcloudcc executable").size(15.0).strong());
                ui.add_space(8.0);
                ui.add(egui::TextEdit::singleline(&mut self.binary_path).desired_width(ui.available_width()));
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("Find automatically")).clicked() {
                    self.binary_path = find_pcloudcc_binary();
                    self.notice = Some((true, "Detected path filled in. Select Use this executable to apply it.".to_string()));
                }
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("Use this executable")).clicked() {
                    self.config.binary_path = self.binary_path.trim().to_string();
                    match self.request_tx.send(Operation::SetBinaryPath(self.binary_path.clone())) {
                        Ok(()) => {
                            self.save_config();
                            if self.notice.as_ref().is_some_and(|(success, _)| *success) {
                                self.notice = Some((true, "Executable path updated and settings saved.".to_string()));
                            }
                        }
                        Err(error) => {
                            self.notice = Some((false, format!("Could not update executable path: {error}")));
                        }
                    }
                }
                ui.add_space(16.0);
                if ui.button("Save settings").clicked() {
                    self.save_config();
                }
            });
    }
}

impl PCloudGui {
    fn update_background(&mut self) -> Duration {
        self.poll_results();
        self.poll_folder_results();
        if self.tfa_armed
            && !self.close_requested
            && matches!(self.status, ProcessStatus::Running)
            && should_send_armed_tfa(daemon_status(&self.status_output), &self.tfa_code)
            && self.busy.is_none()
        {
            self.tfa_armed = false;
            self.queue(Operation::Tfa(self.tfa_code.clone()));
        } else if self.tfa_armed
            && !self.close_requested
            && matches!(
                daemon_status(&self.status_output),
                Some("READY" | "UPLOADING" | "DOWNLOADING" | "DOWNLOADINGANDUPLOADING")
            )
            && self.busy.is_none()
        {
            self.tfa_armed = false;
            self.tfa_code.clear();
            self.notice = Some((
                true,
                "Signed in; pCloud did not request the 2FA code.".to_string(),
            ));
        }
        let refresh_interval = if matches!(self.status, ProcessStatus::Running) {
            status_refresh_interval(daemon_status(&self.status_output))
        } else {
            Duration::from_secs(15)
        };
        if self.status_checked
            && !self.close_requested
            && self.last_status_refresh.elapsed() >= refresh_interval
            && self.busy.is_none()
        {
            self.queue(Operation::Status);
        }
        refresh_interval
    }
}

impl PCloudGui {
    fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let refresh_interval = self.update_background();
        if self.handle_close(&ctx) {
            return;
        }
        self.handle_tray_events(&ctx);
        self.handle_minimized(&ctx, ctx.input(|input| input.viewport().minimized));

        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show(ui, |ui| self.header(ui));
        self.sidebar(ui);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(ui.style())
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(22)),
            )
            .show(ui, |ui| {
                if let Some((success, message)) = self.notice.clone() {
                    let color = if success { SUCCESS } else { ERROR };
                    ui.horizontal(|ui| {
                        ui.colored_label(color, if success { "✓" } else { "!" });
                        ui.colored_label(color, &message);
                        if ui.small_button("×").clicked() {
                            self.notice = None;
                        }
                    });
                    ui.add_space(8.0);
                }
                egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Overview => self.overview_page(ui),
                    Page::Sync => self.sync_page(ui),
                    Page::Crypto => self.crypto_page(ui),
                    Page::Settings => self.settings_page(ui),
                });
            });
        if self.confirm_stop {
            egui::Window::new("Pending transfers")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.set_width(330.0);
                    ui.label("There are pending transfers. Stopping now may interrupt them.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Keep running").clicked() {
                            self.confirm_stop = false;
                            self.close_requested = false;
                        }
                        if ui
                            .add(egui::Button::new("Stop anyway").fill(ERROR))
                            .clicked()
                        {
                            self.confirm_stop = false;
                            self.queue(Operation::Stop(true));
                        }
                    });
                });
        }
        if self.busy.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.status_checked && !self.close_requested {
            ctx.request_repaint_after(
                refresh_interval.saturating_sub(self.last_status_refresh.elapsed()),
            );
        }
    }
}

fn execute_operation(
    manager: &mut ProcessManager,
    operation: Operation,
) -> (ActionKind, Result<String, String>) {
    match operation {
        Operation::SetBinaryPath(_) => unreachable!(),
        Operation::Status => (ActionKind::Status, manager.status()),
        Operation::Folders => (ActionKind::Folders, manager.list_sync_folders()),
        Operation::Start(config, password) => {
            let result = manager.start(&config, &password);
            (ActionKind::Start, result)
        }
        Operation::Stop(confirm_pending) => (ActionKind::Stop, manager.stop(confirm_pending)),
        Operation::Pause => (ActionKind::Pause, manager.pause_sync()),
        Operation::Resume => (ActionKind::Resume, manager.resume_sync()),
        Operation::CryptoStart(password) => {
            let result = manager.start_crypto(&password);
            (ActionKind::CryptoStart, result)
        }
        Operation::CryptoStop => (ActionKind::CryptoStop, manager.stop_crypto()),
        Operation::Auth(password, save) => (ActionKind::Auth, manager.send_auth(&password, save)),
        Operation::Tfa(code) => (ActionKind::Tfa, manager.send_tfa(&code)),
    }
}

fn find_pcloudcc_binary() -> String {
    let mut candidates = vec![PathBuf::from("./pcloudcc"), PathBuf::from("../pcloudcc")];
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join("pcloudcc"));
            // Cargo places the GUI in pcloudcc-gui/target/{debug,release}.
            if let Some(repository) = directory.ancestors().nth(3) {
                candidates.push(repository.join("pcloudcc"));
            }
        }
    }
    candidates.extend([
        PathBuf::from("/usr/local/bin/pcloudcc"),
        PathBuf::from("/usr/bin/pcloudcc"),
    ]);
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin/pcloudcc"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|directory| directory.join("pcloudcc")));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .map(|path| {
            path.canonicalize()
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "./pcloudcc".to_string())
}

fn config_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("pcloudcc-gui").join("settings.json"))
}

fn load_config(path: Option<&std::path::Path>) -> (PCloudConfig, Option<String>) {
    let Some(path) = path else {
        return (
            PCloudConfig::default(),
            Some(
                "Could not determine a configuration directory; settings will not persist."
                    .to_string(),
            ),
        );
    };
    match std::fs::read(path) {
        Ok(data) => match serde_json::from_slice(&data) {
            Ok(config) => (config, None),
            Err(error) => (
                PCloudConfig::default(),
                Some(format!("Could not read saved settings: {error}")),
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (PCloudConfig::default(), None)
        }
        Err(error) => (
            PCloudConfig::default(),
            Some(format!("Could not read saved settings: {error}")),
        ),
    }
}

fn set_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = PANEL;
    visuals.panel_fill = BACKGROUND;
    visuals.extreme_bg_color = egui::Color32::from_rgb(17, 20, 26);
    visuals.faint_bg_color = egui::Color32::from_rgb(34, 39, 48);
    visuals.selection.bg_fill = SELECTED;
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(42, 48, 59);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(53, 61, 75);
    visuals.widgets.active.bg_fill = SELECTED;
    ctx.set_visuals(visuals);
}

fn metric_card(ui: &mut egui::Ui, label: &str, value: impl ToString) {
    egui::Frame::group(ui.style())
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(9)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).size(10.0).strong().color(MUTED));
            ui.add_space(5.0);
            ui.label(egui::RichText::new(value.to_string()).size(15.0).strong());
        });
}

fn form_row(ui: &mut egui::Ui, label: &str, add_field: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [105.0, 25.0],
            egui::Label::new(egui::RichText::new(label).color(MUTED)),
        );
        add_field(ui);
    });
    ui.add_space(8.0);
}

fn empty_state(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(35.0);
        ui.label(egui::RichText::new(title).strong());
        ui.add_space(5.0);
        ui.label(egui::RichText::new(subtitle).color(MUTED));
        ui.add_space(35.0);
    });
}

fn primary_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(
        egui::RichText::new(label)
            .strong()
            .color(egui::Color32::WHITE),
    )
    .fill(ACCENT)
    .min_size([130.0, 34.0].into())
}

fn display_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn daemon_status(output: &str) -> Option<&str> {
    status_field(output, "Status:")
}

fn tray_connected(status: &ProcessStatus, output: &str) -> bool {
    matches!(status, ProcessStatus::Running)
        && matches!(
            daemon_status(output),
            Some(
                "READY"
                    | "UPLOADING"
                    | "DOWNLOADING"
                    | "DOWNLOADINGANDUPLOADING"
                    | "ACCOUNT_FULL"
                    | "DISK_FULL"
            )
        )
}

fn status_field<'a>(output: &'a str, field: &str) -> Option<&'a str> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(field).map(str::trim))
}

fn should_send_armed_tfa(status: Option<&str>, code: &str) -> bool {
    status == Some("TFA_REQUIRED") && !code.is_empty()
}

fn status_refresh_interval(status: Option<&str>) -> Duration {
    match status {
        Some("READY" | "UPLOADING" | "DOWNLOADING" | "DOWNLOADINGANDUPLOADING" | "PAUSED") => {
            Duration::from_secs(15)
        }
        _ => Duration::from_secs(3),
    }
}

fn status_description(status: Option<&str>) -> (&'static str, &'static str, egui::Color32) {
    match status {
        Some("READY") => (
            "Ready",
            "Signed in and ready. Mount availability is not checked here.",
            SUCCESS,
        ),
        Some("DOWNLOADING" | "UPLOADING" | "DOWNLOADINGANDUPLOADING") => {
            ("Syncing", "Transfers are in progress.", SUCCESS)
        }
        Some("CONNECTING") => (
            "Connecting",
            "Connecting to pCloud; this can take a moment.",
            WARNING,
        ),
        Some("SCANNING") => ("Scanning", "Checking local and remote files.", WARNING),
        Some("LOGIN_REQUIRED") => (
            "Sign-in required",
            "Enter your account password and send it to the daemon.",
            WARNING,
        ),
        Some("BAD_LOGIN_DATA" | "BAD_LOGIN_TOKEN") => (
            "Sign-in rejected",
            "Check your account email and password, then try again.",
            ERROR,
        ),
        Some("TFA_REQUIRED") => (
            "2FA required",
            "Enter the code requested by pCloud.",
            WARNING,
        ),
        Some("BAD_TFA_CODE") => (
            "2FA code rejected",
            "Enter a new code and send it again.",
            ERROR,
        ),
        Some("OFFLINE") => ("Offline", "Check your network connection.", WARNING),
        Some("PAUSED") => ("Sync paused", "Resume sync to continue transfers.", WARNING),
        Some("STOPPED") => (
            "Sync stopped",
            "The daemon reports that syncing is stopped.",
            WARNING,
        ),
        Some("ACCOUNT_FULL") => (
            "Account storage full",
            "Free up pCloud storage to resume transfers.",
            ERROR,
        ),
        Some("DISK_FULL") => (
            "Local storage full",
            "Free up local disk space to resume transfers.",
            ERROR,
        ),
        Some("USER_MISMATCH") => (
            "Account mismatch",
            "The daemon reports an account mismatch. Check the configured account.",
            ERROR,
        ),
        Some("ACCOUT_EXPIRED") => (
            "Account expired",
            "Check your pCloud account status.",
            ERROR,
        ),
        Some(_) | None => (
            "Unknown status",
            "See technical details for the daemon response.",
            WARNING,
        ),
    }
}

fn nonempty_message(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(24, 28, 35);
const PANEL: egui::Color32 = egui::Color32::from_rgb(31, 36, 45);
const SELECTED: egui::Color32 = egui::Color32::from_rgb(45, 93, 164);
const BORDER: egui::Color32 = egui::Color32::from_rgb(53, 61, 74);
const TEXT: egui::Color32 = egui::Color32::from_rgb(232, 237, 245);
const MUTED: egui::Color32 = egui::Color32::from_rgb(157, 168, 184);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(76, 148, 245);
const SUCCESS: egui::Color32 = egui::Color32::from_rgb(85, 201, 145);
const WARNING: egui::Color32 = egui::Color32::from_rgb(240, 190, 83);
const ERROR: egui::Color32 = egui::Color32::from_rgb(239, 106, 106);

struct WindowApp(Rc<RefCell<Option<PCloudGui>>>);

impl eframe::App for WindowApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0
            .borrow_mut()
            .as_mut()
            .expect("GUI state missing")
            .render(ui);
    }
}

fn main() -> eframe::Result<()> {
    let state: Rc<RefCell<Option<PCloudGui>>> = Rc::new(RefCell::new(None));
    loop {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([960.0, 700.0])
                .with_min_inner_size([720.0, 540.0])
                .with_title("pCloudCC"),
            ..Default::default()
        };
        let window_state = state.clone();
        eframe::run_native(
            "pCloudCC",
            options,
            Box::new(move |cc| {
                let mut app = window_state.borrow_mut();
                if let Some(app) = app.as_mut() {
                    set_theme(&cc.egui_ctx);
                    if let Some(tray) = &app.tray {
                        tray.set_context(cc.egui_ctx.clone());
                    }
                } else {
                    *app = Some(PCloudGui::new(cc));
                }
                drop(app);
                Ok(Box::new(WindowApp(window_state)))
            }),
        )?;

        let mut state = state.borrow_mut();
        let Some(app) = state.as_mut() else {
            return Ok(());
        };
        if !app.hidden_to_tray {
            return Ok(());
        }
        loop {
            app.update_background();
            if !app.hidden_to_tray {
                break;
            }
            let event = match &app.tray {
                Some(tray) if tray.available() => {
                    tray.events.recv_timeout(Duration::from_millis(100))
                }
                _ => Ok(tray::TrayAction::Offline),
            };
            match event {
                Ok(tray::TrayAction::Online) | Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Ok(tray::TrayAction::OpenFolder) => {
                    app.open_folder(None);
                }
                Ok(action) => {
                    app.hidden_to_tray = false;
                    match action {
                        tray::TrayAction::Quit => app.close_requested = true,
                        tray::TrayAction::Offline => {
                            app.notice = Some((
                                false,
                                "System tray disconnected; the window has been restored."
                                    .to_string(),
                            ));
                        }
                        tray::TrayAction::Show => {}
                        tray::TrayAction::Online | tray::TrayAction::OpenFolder => unreachable!(),
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    app.hidden_to_tray = false;
                    app.notice = Some((
                        false,
                        "System tray disconnected; the window has been restored.".to_string(),
                    ));
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> (PCloudGui, Receiver<Operation>, Sender<OperationResult>) {
        let (request_tx, requests) = mpsc::channel();
        let (results, result_rx) = mpsc::channel();
        let (folder_tx, folder_rx) = mpsc::channel();
        (
            PCloudGui {
                page: Page::Overview,
                config: PCloudConfig::default(),
                account_password: String::new(),
                crypto_password: String::new(),
                tfa_code: String::new(),
                tfa_armed: false,
                show_account_password: false,
                show_crypto_password: false,
                binary_path: String::new(),
                config_path: None,
                status: ProcessStatus::Running,
                status_checked: true,
                status_output: "Status: READY".to_string(),
                folders_output: String::new(),
                busy: None,
                confirm_stop: false,
                close_requested: false,
                close_ready: false,
                notice: None,
                request_tx,
                result_rx,
                folder_tx,
                folder_rx,
                last_status_refresh: Instant::now(),
                tray: None,
                hidden_to_tray: false,
                awaiting_unminimize: false,
            },
            requests,
            results,
        )
    }

    fn close_frame(app: &mut PCloudGui, ctx: &egui::Context, request_close: bool) {
        let mut input = egui::RawInput::default();
        if request_close {
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(egui::ViewportEvent::Close);
        }
        ctx.run_pass(input, |ctx| {
            app.poll_results();
            app.handle_close(ctx);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn tray_restore_and_disconnect_make_window_visible() {
        let (mut app, _, _) = test_app();
        let ctx = egui::Context::default();
        app.hidden_to_tray = true;
        app.handle_tray_action(&ctx, tray::TrayAction::Show);
        assert!(!app.hidden_to_tray);
        assert!(!app.close_requested);

        app.hidden_to_tray = true;
        app.handle_tray_action(&ctx, tray::TrayAction::Offline);
        assert!(!app.hidden_to_tray);
        assert!(app.notice.as_ref().is_some_and(|(success, _)| !success));
    }

    #[test]
    fn folder_results_only_restore_window_on_error() {
        let (mut app, _, _) = test_app();
        app.hidden_to_tray = true;
        app.folder_tx.send(Ok(())).unwrap();
        app.poll_folder_results();
        assert!(app.hidden_to_tray);
        assert!(app.notice.is_none());

        app.folder_tx.send(Err("open failed".into())).unwrap();
        app.poll_folder_results();
        assert!(!app.hidden_to_tray);
        assert_eq!(app.notice, Some((false, "open failed".into())));
    }

    #[test]
    fn tray_quit_uses_existing_graceful_stop_flow() {
        let (mut app, requests, _results) = test_app();
        let ctx = egui::Context::default();
        app.hidden_to_tray = true;
        app.handle_tray_action(&ctx, tray::TrayAction::Quit);
        assert!(!app.hidden_to_tray);
        assert!(app.close_requested);
        close_frame(&mut app, &ctx, false);
        assert!(matches!(requests.try_recv(), Ok(Operation::Stop(false))));
    }

    #[test]
    fn minimizing_without_a_tray_does_not_hide_the_window() {
        let (mut app, _, _) = test_app();
        app.minimize_to_tray(&egui::Context::default());
        assert!(!app.hidden_to_tray);
    }

    #[test]
    fn tray_hide_and_show_issue_visibility_commands() {
        let (mut app, _, _) = test_app();
        let ctx = egui::Context::default();
        let hidden = ctx.run_pass(egui::RawInput::default(), |ctx| {
            app.hide_window_for_tray(ctx);
        });
        assert!(app.hidden_to_tray);
        assert!(hidden.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::Close));
        hidden.drop_without_applying_deltas();

        let shown = ctx.run_pass(egui::RawInput::default(), |ctx| {
            app.handle_tray_action(ctx, tray::TrayAction::Show);
        });
        assert!(!app.hidden_to_tray);
        let commands = &shown.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands.contains(&egui::ViewportCommand::Visible(true)));
        assert!(commands.contains(&egui::ViewportCommand::Minimized(false)));
        shown.drop_without_applying_deltas();
        app.handle_minimized(&ctx, Some(true));
        assert!(app.awaiting_unminimize);
        assert!(!app.hidden_to_tray);
        app.handle_minimized(&ctx, Some(false));
        assert!(!app.awaiting_unminimize);
    }

    #[test]
    fn tray_hide_closes_only_window_and_preserves_state() {
        let (mut app, requests, _results) = test_app();
        let ctx = egui::Context::default();
        app.account_password = "test password".to_string();
        app.hidden_to_tray = true;
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = ctx.run_pass(input, |ctx| assert!(app.handle_close(ctx)));
        assert!(!output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::CancelClose));
        output.drop_without_applying_deltas();
        assert!(requests.try_recv().is_err());
        assert!(!app.close_requested);
        assert_eq!(app.account_password, "test password");
    }

    #[test]
    fn window_close_hides_to_available_tray_without_stopping_daemon() {
        let (mut app, requests, _results) = test_app();
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = ctx.run_pass(input, |ctx| assert!(app.handle_close_with_tray(ctx, true)));
        assert!(app.hidden_to_tray);
        assert!(!app.close_requested);
        assert!(requests.try_recv().is_err());
        assert!(!output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::CancelClose));
        output.drop_without_applying_deltas();
    }

    #[test]
    fn window_close_without_tray_remains_open_and_explains_exit() {
        let (mut app, requests, _results) = test_app();
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = ctx.run_pass(input, |ctx| assert!(!app.handle_close(ctx)));
        assert!(output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::CancelClose));
        output.drop_without_applying_deltas();
        assert!(!app.hidden_to_tray);
        assert!(!app.close_requested);
        assert!(requests.try_recv().is_err());
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(_, message)| message.contains("Use Exit")));
    }

    #[test]
    fn background_authentication_continues_while_hidden() {
        let (mut app, requests, _results) = test_app();
        app.hidden_to_tray = true;
        app.status_output = "Status: TFA_REQUIRED".to_string();
        app.tfa_armed = true;
        app.tfa_code = "123456".to_string();
        app.update_background();
        assert!(matches!(requests.try_recv(), Ok(Operation::Tfa(code)) if code == "123456"));
    }

    #[test]
    fn tray_connection_requires_an_online_status() {
        for value in [
            "READY",
            "UPLOADING",
            "DOWNLOADING",
            "DOWNLOADINGANDUPLOADING",
            "ACCOUNT_FULL",
            "DISK_FULL",
        ] {
            assert!(tray_connected(
                &ProcessStatus::Running,
                &format!("Status: {value}")
            ));
        }
        for value in [
            "CONNECTING",
            "SCANNING",
            "OFFLINE",
            "LOGIN_REQUIRED",
            "TFA_REQUIRED",
            "PAUSED",
            "STOPPED",
            "UNKNOWN",
        ] {
            assert!(!tray_connected(
                &ProcessStatus::Running,
                &format!("Status: {value}")
            ));
        }
        assert!(!tray_connected(&ProcessStatus::NotRunning, "Status: READY"));
        assert!(!tray_connected(
            &ProcessStatus::Error("failure".into()),
            "Status: READY"
        ));
        assert!(!tray_connected(&ProcessStatus::Running, ""));
    }

    #[test]
    fn disconnected_daemon_is_rechecked_in_background() {
        let (mut app, requests, _results) = test_app();
        app.status = ProcessStatus::NotRunning;
        app.last_status_refresh = Instant::now() - Duration::from_secs(16);
        app.hidden_to_tray = true;
        app.update_background();
        assert!(matches!(requests.try_recv(), Ok(Operation::Status)));
    }

    #[test]
    fn exiting_stops_daemon_before_closing_window() {
        let (mut app, requests, results) = test_app();
        let ctx = egui::Context::default();
        app.request_exit(&ctx);
        close_frame(&mut app, &ctx, false);
        assert!(app.close_requested);
        assert!(!app.close_ready);
        assert!(matches!(requests.try_recv(), Ok(Operation::Stop(false))));

        results
            .send(OperationResult {
                action: ActionKind::Stop,
                result: Ok("Exiting ...".into()),
            })
            .unwrap();
        close_frame(&mut app, &ctx, false);
        assert!(app.close_ready);
    }

    #[test]
    fn exiting_waits_for_inflight_start() {
        let (mut app, requests, results) = test_app();
        app.busy = Some(ActionKind::Start);
        let ctx = egui::Context::default();
        app.request_exit(&ctx);
        close_frame(&mut app, &ctx, false);
        assert!(requests.try_recv().is_err());
        results
            .send(OperationResult {
                action: ActionKind::Start,
                result: Ok("Status: READY".into()),
            })
            .unwrap();
        close_frame(&mut app, &ctx, false);
        assert!(matches!(requests.try_recv(), Ok(Operation::Stop(false))));
    }

    #[test]
    fn exiting_preserves_pending_transfer_confirmation_and_errors() {
        let (mut app, requests, results) = test_app();
        let ctx = egui::Context::default();
        app.request_exit(&ctx);
        close_frame(&mut app, &ctx, false);
        assert!(matches!(requests.try_recv(), Ok(Operation::Stop(false))));
        results
            .send(OperationResult {
                action: ActionKind::Stop,
                result: Err("Stopping may interrupt pending transfers.".into()),
            })
            .unwrap();
        close_frame(&mut app, &ctx, false);
        assert!(app.confirm_stop);
        assert!(!app.close_ready);
        assert!(requests.try_recv().is_err());

        app.confirm_stop = false;
        app.queue(Operation::Stop(true));
        assert!(matches!(requests.try_recv(), Ok(Operation::Stop(true))));
        results
            .send(OperationResult {
                action: ActionKind::Stop,
                result: Err("Shutdown failed".into()),
            })
            .unwrap();
        close_frame(&mut app, &ctx, false);
        assert!(!app.close_requested);
        assert!(!app.close_ready);
        assert_eq!(app.notice, Some((false, "Shutdown failed".into())));
    }

    #[test]
    fn status_fields_ignore_command_prompt() {
        let output = "pcloud> status\nStatus:   SCANNING\nDownload: Everything Downloaded\nUpload:   Everything Uploaded\npcloud> quit";
        assert_eq!(daemon_status(output), Some("SCANNING"));
        assert_eq!(
            status_field(output, "Download:"),
            Some("Everything Downloaded")
        );
        assert_eq!(status_field(output, "Upload:"), Some("Everything Uploaded"));
        assert_eq!(status_description(daemon_status(output)).0, "Scanning");
    }

    #[test]
    fn status_distinguishes_ready_login_and_unknown() {
        assert_eq!(status_description(Some("READY")).0, "Ready");
        assert_eq!(
            status_description(Some("BAD_LOGIN_DATA")).0,
            "Sign-in rejected"
        );
        assert_eq!(status_description(Some("TFA_REQUIRED")).0, "2FA required");
        assert_eq!(
            status_description(Some("BAD_TFA_CODE")).0,
            "2FA code rejected"
        );
        assert_eq!(status_description(Some("NEW_STATE")).0, "Unknown status");
    }

    #[test]
    fn preentered_tfa_is_only_sent_for_a_fresh_challenge() {
        assert!(should_send_armed_tfa(Some("TFA_REQUIRED"), "123456"));
        for status in [
            None,
            Some("CONNECTING"),
            Some("LOGIN_REQUIRED"),
            Some("READY"),
            Some("BAD_TFA_CODE"),
        ] {
            assert!(!should_send_armed_tfa(status, "123456"));
        }
        assert!(!should_send_armed_tfa(Some("TFA_REQUIRED"), ""));
    }

    #[test]
    fn transitional_status_refreshes_more_often() {
        assert_eq!(
            status_refresh_interval(Some("SCANNING")),
            Duration::from_secs(3)
        );
        assert_eq!(
            status_refresh_interval(Some("TFA_REQUIRED")),
            Duration::from_secs(3)
        );
        assert_eq!(
            status_refresh_interval(Some("READY")),
            Duration::from_secs(15)
        );
    }
}
