use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::{
    App, CreationContext,
    egui::{self, Color32, RichText},
};

use crate::{
    capture,
    network::{self, ConnectionUrl, UrlSet},
    state::AppState,
};

pub struct RustOpViewerApp {
    state: Arc<AppState>,
    urls: UrlSet,
    last_network_refresh: Instant,
    toast_message: Option<(String, Instant)>,
}

impl RustOpViewerApp {
    pub fn new(cc: &CreationContext<'_>, state: Arc<AppState>) -> Self {
        configure_theme(&cc.egui_ctx);

        Self {
            urls: network::discover_urls(state.port(), &state.auth_token()),
            state,
            last_network_refresh: Instant::now(),
            toast_message: None,
        }
    }

    fn refresh_urls(&mut self) {
        self.urls = network::discover_urls(self.state.port(), &self.state.auth_token());
        self.last_network_refresh = Instant::now();
    }

    fn maybe_refresh_urls(&mut self) {
        if self.last_network_refresh.elapsed() >= Duration::from_secs(5) {
            self.refresh_urls();
        }
    }

    fn copy_text(&mut self, ctx: &egui::Context, label: &str, value: String) {
        ctx.copy_text(value);
        self.toast_message = Some((format!("{label} copied"), Instant::now()));
    }
}

impl App for RustOpViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.maybe_refresh_urls();
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(400));

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading(
                    RichText::new("RustOp Viewer")
                        .size(30.0)
                        .color(Color32::from_rgb(241, 245, 249)),
                );
                ui.label(
                    RichText::new(
                        "Remote desktop viewing and full mouse/keyboard control for Windows 11 over Tailscale or your LAN.",
                    )
                    .size(15.0)
                    .color(Color32::from_rgb(148, 163, 184)),
                );
            });

            if let Some((message, created_at)) = &self.toast_message
                && created_at.elapsed() < Duration::from_secs(2)
            {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(message)
                        .color(Color32::from_rgb(245, 158, 11))
                        .strong(),
                );
            }

            ui.add_space(10.0);
            ui.columns(2, |columns| {
                let (left_columns, right_columns) = columns.split_at_mut(1);
                let left = &mut left_columns[0];
                let right = &mut right_columns[0];

                left.group(|ui| {
                    ui.heading("Desktop Status");
                    ui.label(format!("Listening on port {}", self.state.port()));

                    if let Some(frame) = self.state.latest_frame() {
                        let age_ms = frame
                            .captured_at
                            .elapsed()
                            .map(|elapsed| elapsed.as_millis())
                            .unwrap_or(0);
                        ui.label(format!(
                            "Latest frame: {}x{} (source {}x{}, {} ms ago)",
                            frame.encoded_width,
                            frame.encoded_height,
                            frame.source_width,
                            frame.source_height,
                            age_ms
                        ));
                    } else {
                        ui.label("Latest frame: waiting for the first capture");
                    }

                    if let Some(error) = self.state.capture_error() {
                        ui.colored_label(Color32::from_rgb(248, 113, 113), error);
                    } else {
                        ui.colored_label(Color32::from_rgb(74, 222, 128), "Capture loop is healthy");
                    }

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Use a Tailscale URL below from Safari on your iPhone for remote access away from home.")
                            .color(Color32::from_rgb(226, 232, 240)),
                    );
                });

                left.add_space(12.0);
                left.group(|ui| {
                    ui.heading("Monitor");
                    let monitors = self.state.monitors();
                    let mut selected_monitor_id = self.state.selected_monitor_id();
                    let selected_text = self
                        .state
                        .selected_monitor()
                        .map(|monitor| format!("{} • {}", monitor.display_name(), monitor.resolution_label()))
                        .unwrap_or_else(|| "No display detected".to_string());

                    egui::ComboBox::from_id_salt("monitor-select")
                        .selected_text(selected_text)
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for monitor in &monitors {
                                let response = ui.selectable_value(
                                    &mut selected_monitor_id,
                                    Some(monitor.id),
                                    format!(
                                        "{} • {} @ {},{}",
                                        monitor.display_name(),
                                        monitor.resolution_label(),
                                        monitor.x,
                                        monitor.y
                                    ),
                                );

                                if response.clicked()
                                    && let Err(err) = self.state.set_selected_monitor(monitor.id)
                                {
                                    self.toast_message =
                                        Some((format!("Failed to save monitor: {err}"), Instant::now()));
                                }
                            }
                        });

                    ui.horizontal(|ui| {
                        if ui.button("Refresh Displays").clicked() {
                            match capture::discover_monitors() {
                                Ok(monitors) => {
                                    self.state.set_monitors(monitors);
                                    if let Err(err) = self.state.ensure_valid_selected_monitor() {
                                        self.toast_message = Some((
                                            format!("Monitor refresh failed: {err}"),
                                            Instant::now(),
                                        ));
                                    }
                                }
                                Err(err) => {
                                    self.toast_message = Some((
                                        format!("Monitor refresh failed: {err}"),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }

                        if ui.button("Regenerate Secure Link").clicked() {
                            match self.state.regenerate_auth_token() {
                                Ok(_) => {
                                    self.refresh_urls();
                                    self.toast_message = Some((
                                        "Phone link rotated; old links now stop working".to_string(),
                                        Instant::now(),
                                    ));
                                }
                                Err(err) => {
                                    self.toast_message = Some((
                                        format!("Token rotation failed: {err}"),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                    });

                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Run this app as Administrator if you need to click or type into elevated Windows prompts or apps.",
                        )
                        .color(Color32::from_rgb(248, 250, 252)),
                    );
                    ui.label(
                        RichText::new(
                            "Ctrl+Alt+Del cannot be synthesized from a normal user-space app, so Windows secure attention stays out of scope.",
                        )
                        .color(Color32::from_rgb(148, 163, 184)),
                    );
                });

                right.group(|ui| {
                    ui.heading("Connection URLs");
                    ui.label(
                        RichText::new("Preferred direct URL (HTTP)")
                            .color(Color32::from_rgb(148, 163, 184))
                            .strong(),
                    );
                    render_url_row(ui, &ctx, &mut self.toast_message, &self.urls.preferred);

                    if !self.urls.tailscale.is_empty() {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Tailscale")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        for url in &self.urls.tailscale {
                            render_url_row(ui, &ctx, &mut self.toast_message, url);
                        }
                    }

                    if !self.urls.lan.is_empty() {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Local network")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        for url in &self.urls.lan {
                            render_url_row(ui, &ctx, &mut self.toast_message, url);
                        }
                    }

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Same machine")
                            .color(Color32::from_rgb(148, 163, 184))
                            .strong(),
                    );
                    render_url_row(ui, &ctx, &mut self.toast_message, &self.urls.loopback);

                    ui.horizontal(|ui| {
                        if ui.button("Refresh Network").clicked() {
                            self.refresh_urls();
                        }

                        if ui.button("Copy Preferred URL").clicked() {
                            self.copy_text(&ctx, "Preferred URL", self.urls.preferred.url.clone());
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("HTTPS via Tailscale Serve")
                            .color(Color32::from_rgb(148, 163, 184))
                            .strong(),
                    );
                    ui.label(
                        RichText::new(
                            "Enable MagicDNS and HTTPS certificates in Tailscale, then run this on the Windows host:",
                        )
                        .color(Color32::from_rgb(226, 232, 240)),
                    );

                    if let Some(url) = &self.urls.tailscale_https {
                        render_url_row(ui, &ctx, &mut self.toast_message, url);
                    }

                    let serve_command = format!("tailscale serve --bg {}", self.state.port());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(&serve_command)
                                .monospace()
                                .color(Color32::from_rgb(191, 219, 254)),
                        );
                        if ui.small_button("Copy").clicked() {
                            self.copy_text(
                                &ctx,
                                "Tailscale Serve command",
                                serve_command.clone(),
                            );
                        }
                    });
                    ui.label(
                        RichText::new(
                            "Tailscale will print a trusted https://...ts.net URL that proxies back to ROV locally.",
                        )
                        .small()
                        .color(Color32::from_rgb(100, 116, 139)),
                    );
                });

                right.add_space(12.0);
                right.group(|ui| {
                    ui.heading("How to Use It");
                    ui.label("1. Start Tailscale on both the laptop and your iPhone.");
                    ui.label("2. Open one of the direct Tailscale URLs above in Safari on the phone, or use the HTTPS Serve URL after you enable it.");
                    ui.label("3. Tap the live image for left click, long-press for right click, and use Drag mode when you need to hold the mouse button down.");
                    ui.label("4. Use the text box and shortcut buttons on the phone page for typing and common Windows commands.");
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!("Config file: {}", self.state.config_path().display()))
                            .small()
                            .color(Color32::from_rgb(100, 116, 139)),
                    );
                });
            });
        });
    }
}

fn render_url_row(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    toast_message: &mut Option<(String, Instant)>,
    connection: &ConnectionUrl,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&connection.label)
                .strong()
                .color(Color32::from_rgb(248, 250, 252)),
        );
        ui.label(
            RichText::new(&connection.url)
                .monospace()
                .color(Color32::from_rgb(191, 219, 254)),
        );
        if ui.small_button("Copy").clicked() {
            ctx.copy_text(connection.url.clone());
            *toast_message = Some((format!("{} copied", connection.label), Instant::now()));
        }
    });
}

fn configure_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(10, 15, 23);
    visuals.window_fill = Color32::from_rgb(15, 23, 34);
    visuals.extreme_bg_color = Color32::from_rgb(18, 28, 42);
    visuals.faint_bg_color = Color32::from_rgb(20, 31, 46);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(17, 26, 38);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(25, 40, 60);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(32, 54, 82);
    visuals.widgets.active.bg_fill = Color32::from_rgb(40, 70, 105);
    visuals.override_text_color = Some(Color32::from_rgb(226, 232, 240));
    ctx.set_visuals(visuals);

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.visuals.selection.bg_fill = Color32::from_rgb(11, 98, 217);
    ctx.set_global_style(style);
}
