use eframe::egui;
use ksni::blocking::TrayMethods;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    OpenFolder,
    Quit,
    Offline,
    Online,
}

pub struct Tray {
    handle: ksni::blocking::Handle<TrayItem>,
    online: Arc<AtomicBool>,
    context: Arc<Mutex<egui::Context>>,
    connected: Cell<bool>,
    pub events: Receiver<TrayAction>,
}

struct TrayItem {
    sender: Sender<TrayAction>,
    context: Arc<Mutex<egui::Context>>,
    online: Arc<AtomicBool>,
    connected: bool,
}

impl TrayItem {
    fn send(&self, action: TrayAction) {
        if self.sender.send(action).is_ok() {
            self.context
                .lock()
                .expect("tray context poisoned")
                .request_repaint();
        }
    }
}

impl ksni::Tray for TrayItem {
    fn id(&self) -> String {
        "pcloudcc-gui".to_string()
    }

    fn title(&self) -> String {
        if self.connected {
            "pCloudCC - connected"
        } else {
            "pCloudCC - disconnected or not ready"
        }
        .to_string()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![status_icon(self.connected)]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayAction::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Show pCloudCC".to_string(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayAction::Show)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open pCloud folder".to_string(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayAction::OpenFolder)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit and stop pCloudCC".to_string(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayAction::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.online.store(false, Ordering::SeqCst);
        self.send(TrayAction::Offline);
        true
    }

    fn watcher_online(&self) {
        self.online.store(true, Ordering::SeqCst);
        self.send(TrayAction::Online);
    }
}

fn status_icon(connected: bool) -> ksni::Icon {
    let mut data = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32_i32 {
        for x in 0..32_i32 {
            let inside = (x - 16).pow(2) + (y - 16).pow(2) <= 225;
            let cloud = ((x - 12).pow(2) + (y - 18).pow(2) <= 16)
                || ((x - 17).pow(2) + (y - 14).pow(2) <= 25)
                || ((x - 22).pow(2) + (y - 18).pow(2) <= 16)
                || ((11..=22).contains(&x) && (18..=21).contains(&y));
            let badge = (x - 24).pow(2) + (y - 24).pow(2) <= 49;
            let symbol = if connected {
                ((20..=23).contains(&x) && (y - (x + 2)).abs() <= 1)
                    || ((23..=28).contains(&x) && (y - (48 - x)).abs() <= 1)
            } else {
                (21..=27).contains(&x)
                    && (21..=27).contains(&y)
                    && ((x - y).abs() <= 1 || (x + y - 48).abs() <= 1)
            };
            data.extend_from_slice(if badge && symbol {
                &[255, 255, 255, 255]
            } else if badge && connected {
                &[255, 32, 164, 92]
            } else if badge {
                &[255, 195, 59, 59]
            } else if cloud {
                &[255, 255, 255, 255]
            } else if inside && !connected {
                &[255, 105, 113, 124]
            } else if inside {
                &[255, 42, 127, 224]
            } else {
                &[0, 0, 0, 0]
            });
        }
    }
    ksni::Icon {
        width: 32,
        height: 32,
        data,
    }
}

impl Tray {
    pub fn new(context: egui::Context) -> Result<Self, String> {
        let (sender, events) = mpsc::channel();
        let online = Arc::new(AtomicBool::new(true));
        let context = Arc::new(Mutex::new(context));
        let handle = TrayItem {
            sender,
            context: context.clone(),
            online: online.clone(),
            connected: false,
        }
        .spawn()
        .map_err(|error| format!("System tray unavailable: {error}"))?;
        Ok(Self {
            handle,
            online,
            context,
            connected: Cell::new(false),
            events,
        })
    }

    pub fn set_context(&self, context: egui::Context) {
        *self.context.lock().expect("tray context poisoned") = context;
    }

    pub fn available(&self) -> bool {
        self.online.load(Ordering::SeqCst) && !self.handle.is_closed()
    }

    pub fn set_connected(&self, connected: bool) {
        if self.connected.get() != connected {
            if self
                .handle
                .update(|item| item.connected = connected)
                .is_some()
            {
                self.connected.set(connected);
            } else {
                eprintln!("Could not update the system tray icon: tray service has stopped.");
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.handle.shutdown().wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_icons_are_distinct_and_have_correct_format() {
        let connected = status_icon(true);
        let disconnected = status_icon(false);
        assert_eq!((connected.width, connected.height), (32, 32));
        assert_eq!(connected.data.len(), 32 * 32 * 4);
        assert_eq!(disconnected.data.len(), 32 * 32 * 4);
        assert_ne!(connected.data, disconnected.data);
    }
}
