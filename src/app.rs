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
    network::{self, ConnectionUrl, RemoteAccessMode, TailscaleStatusSnapshot, UrlSet},
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
            urls: network::discover_urls(state.port()),
            state,
            last_network_refresh: Instant::now(),
            toast_message: None,
        }
    }

    fn refresh_urls(&mut self) {
        self.urls = network::discover_urls(self.state.port());
        self.last_network_refresh = Instant::now();
    }

    fn maybe_refresh_urls(&mut self) {
        if self.last_network_refresh.elapsed() >= Duration::from_secs(5) {
            self.refresh_urls();
        }
    }

    fn show_toast(&mut self, message: impl Into<String>) {
        self.toast_message = Some((message.into(), Instant::now()));
    }

    fn copy_text(&mut self, ctx: &egui::Context, label: &str, value: String) {
        ctx.copy_text(value);
        self.show_toast(format!("{label} copied"));
    }

    fn enable_tailscale_https(&mut self) {
        match network::enable_tailscale_https(self.state.port()) {
            Ok(()) => {
                self.refresh_urls();
                self.show_toast(
                    "Trusted Tailscale HTTPS is ready. Open the phone URL and pair with a fresh code.",
                );
            }
            Err(err) => {
                self.show_toast(format!("Tailscale HTTPS setup failed: {err}"));
            }
        }
    }

    fn render_remote_access_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let status = self.urls.tailscale_status.clone();
        let mobile_url = self.urls.mobile_data_preferred.clone();
        let https_url = self.urls.tailscale_https.clone();
        let serve_command = format!("tailscale serve --bg --yes {}", self.state.port());
        let (headline, detail, accent) = remote_access_copy(&status);

        ui.heading("Off-LAN Access");
        ui.label(
            RichText::new(
                "ROV now keeps its own server on 127.0.0.1 and expects Tailscale Serve to proxy the phone session in.",
            )
            .color(Color32::from_rgb(148, 163, 184)),
        );
        ui.add_space(6.0);
        ui.label(RichText::new(headline).color(accent).strong());
        ui.label(RichText::new(detail).color(Color32::from_rgb(226, 232, 240)));

        if let Some(url) = mobile_url.as_ref() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Phone URL")
                    .color(Color32::from_rgb(148, 163, 184))
                    .strong(),
            );
            render_url_row(ui, ctx, &mut self.toast_message, url);
        }

        if let Some(url) = https_url.as_ref().filter(|candidate| {
            mobile_url
                .as_ref()
                .is_none_or(|selected| selected.url != candidate.url)
        }) {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Trusted HTTPS")
                    .color(Color32::from_rgb(148, 163, 184))
                    .strong(),
            );
            render_url_row(ui, ctx, &mut self.toast_message, url);
        }

        if let Some(tailnet_name) = status.tailnet_name.as_deref() {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Tailnet: {tailnet_name}"))
                    .small()
                    .color(Color32::from_rgb(148, 163, 184)),
            );
        }

        if let Some(host_name) = status.host_name.as_deref() {
            ui.label(
                RichText::new(format!("Laptop name on Tailscale: {host_name}"))
                    .small()
                    .color(Color32::from_rgb(148, 163, 184)),
            );
        }

        if !status.tailscale_ips.is_empty() {
            ui.label(
                RichText::new(format!(
                    "Tailscale reported {} tailnet address(es), but ROV intentionally keeps direct tailnet HTTP disabled.",
                    status.tailscale_ips.len()
                ))
                .small()
                .color(Color32::from_rgb(148, 163, 184)),
            );
        }

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Copy Phone URL").clicked() {
                if let Some(url) = mobile_url.as_ref() {
                    self.copy_text(ctx, "Phone URL", url.url.clone());
                } else {
                    self.show_toast(
                        "No off-LAN phone URL is available yet. Turn on Tailscale Serve first.",
                    );
                }
            }

            let can_enable_https = status.is_running
                && status.magic_dns_enabled
                && status.https_certificates_available;
            let https_button_label = if status.serve_enabled {
                "Refresh HTTPS Status"
            } else {
                "Enable HTTPS for iPhone"
            };

            if ui
                .add_enabled(can_enable_https, egui::Button::new(https_button_label))
                .clicked()
            {
                if status.serve_enabled {
                    self.refresh_urls();
                    self.show_toast("HTTPS status refreshed");
                } else {
                    self.enable_tailscale_https();
                }
            }
        });

        match status.remote_access_mode() {
            RemoteAccessMode::ReadyHttps => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Safari can open the HTTPS link directly. Pair the phone with a one-time code shown on this Windows app.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::NeedsServe => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Tailscale is ready, but Serve has not published the HTTPS phone URL yet.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::NeedsTailscaleLogin => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Open Tailscale on the Windows laptop, sign in, and verify the tailnet is healthy before pairing the phone.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::NeedsTailscaleInstall => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Install Tailscale on the laptop and phone first. ROV no longer supports off-LAN exposure without that boundary.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::LocalOnly => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "The server is still local-only. Until Tailscale Serve is ready, there is no phone URL to trust off-LAN.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
        }

        if status.is_running && status.magic_dns_enabled && !status.https_certificates_available {
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Tailscale HTTPS certificates are not enabled for this tailnet yet. Turn on HTTPS certificates in the Tailscale admin DNS settings, then refresh here.",
                )
                .small()
                .color(Color32::from_rgb(245, 158, 11)),
            );
        } else if status.is_running && !status.serve_enabled {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(&serve_command)
                        .monospace()
                        .color(Color32::from_rgb(191, 219, 254)),
                );
                if ui.small_button("Copy").clicked() {
                    self.copy_text(ctx, "Tailscale HTTPS command", serve_command.clone());
                }
            });
        }
    }

    fn render_security_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Security");
        ui.label(
            RichText::new(
                "The phone now pairs with a one-time code. Only one approved device session is kept at a time, and remote control stays off until you enable it here.",
            )
            .color(Color32::from_rgb(226, 232, 240)),
        );

        if self.state.is_elevated() {
            ui.add_space(8.0);
            ui.colored_label(
                Color32::from_rgb(248, 113, 113),
                "This process is elevated. Remote input is locked to view-only until ROV is restarted without Administrator rights.",
            );
        }

        if let Some(session) = self.state.current_remote_session() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Approved phone session")
                    .strong()
                    .color(Color32::from_rgb(248, 250, 252)),
            );
            ui.label(format!(
                "Session expires in {} and idles out in {}.",
                format_duration_compact(session.expires_in),
                format_duration_compact(session.idle_expires_in)
            ));
            if let Some(user_agent) = self.state.current_remote_user_agent() {
                ui.label(
                    RichText::new(format!("User-Agent: {user_agent}"))
                        .small()
                        .color(Color32::from_rgb(148, 163, 184)),
                );
            }
        } else {
            ui.add_space(8.0);
            ui.label(
                RichText::new("No approved phone session is active.")
                    .color(Color32::from_rgb(148, 163, 184)),
            );
        }

        ui.add_space(8.0);
        if let Some(code) = self.state.current_pair_code() {
            ui.label(
                RichText::new("Current pairing code")
                    .strong()
                    .color(Color32::from_rgb(248, 250, 252)),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(&code.code)
                        .monospace()
                        .size(24.0)
                        .color(Color32::from_rgb(191, 219, 254)),
                );
                ui.label(
                    RichText::new(format!(
                        "expires in {} • {} attempt(s) left",
                        format_duration_compact(code.expires_in),
                        code.remaining_attempts
                    ))
                    .small()
                    .color(Color32::from_rgb(148, 163, 184)),
                );
            });
        } else {
            ui.label(
                RichText::new("No pairing code is active.").color(Color32::from_rgb(148, 163, 184)),
            );
        }

        ui.horizontal_wrapped(|ui| {
            if ui.button("Generate Pairing Code").clicked() {
                let snapshot = self.state.generate_pair_code();
                self.show_toast(format!(
                    "Pairing code {} is ready for the next phone session",
                    snapshot.code
                ));
            }

            if ui.button("Disconnect Phone").clicked() {
                self.state.revoke_remote_session();
                self.show_toast("Approved phone session disconnected");
            }

            if ui.button("Panic Stop").clicked() {
                match self.state.panic_stop() {
                    Ok(()) => self
                        .show_toast("Remote input disabled and every pairing/session was cleared"),
                    Err(err) => self.show_toast(format!("Panic stop failed: {err}")),
                }
            }
        });

        ui.add_space(8.0);
        let mut pointer_enabled = self.state.remote_pointer_requested();
        if ui
            .add_enabled(
                !self.state.is_elevated(),
                egui::Checkbox::new(
                    &mut pointer_enabled,
                    "Allow remote pointer, drag, click, and scroll",
                ),
            )
            .changed()
        {
            if let Err(err) = self.state.set_remote_pointer_enabled(pointer_enabled) {
                self.show_toast(format!("Pointer control update failed: {err}"));
            }
        }

        let mut keyboard_enabled = self.state.remote_keyboard_requested();
        if ui
            .add_enabled(
                !self.state.is_elevated(),
                egui::Checkbox::new(
                    &mut keyboard_enabled,
                    "Allow remote keyboard, text, and shortcuts",
                ),
            )
            .changed()
        {
            if let Err(err) = self.state.set_remote_keyboard_enabled(keyboard_enabled) {
                self.show_toast(format!("Keyboard control update failed: {err}"));
            }
        }

        ui.label(
            RichText::new(
                "Leave both boxes off for view-only mode. Turn them on only when you actively need control.",
            )
            .small()
            .color(Color32::from_rgb(148, 163, 184)),
        );
    }
}

impl App for RustOpViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.maybe_refresh_urls();
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(400));

        let preferred_url = self.urls.preferred.clone();
        let mobile_url = self.urls.mobile_data_preferred.clone();
        let loopback_url = self.urls.loopback.clone();

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading(
                    RichText::new("RustOp Viewer")
                        .size(30.0)
                        .color(Color32::from_rgb(241, 245, 249)),
                );
                ui.label(
                    RichText::new(
                        "Remote desktop viewing for Windows 11 with pair-approved phone sessions and Tailscale Serve as the off-LAN boundary.",
                    )
                    .size(15.0)
                    .color(Color32::from_rgb(148, 163, 184)),
                );
            });

            if let Some((message, created_at)) = &self.toast_message
                && created_at.elapsed() < Duration::from_secs(3)
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
                    ui.label(format!(
                        "Listening on local loopback only: 127.0.0.1:{}",
                        self.state.port()
                    ));

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
                        RichText::new(
                            "The Windows app itself no longer opens a LAN-facing control socket. Off-LAN use is meant to arrive through Tailscale Serve only.",
                        )
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
                        .map(|monitor| {
                            format!("{} • {}", monitor.display_name(), monitor.resolution_label())
                        })
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
                                    self.show_toast(format!("Failed to save monitor: {err}"));
                                }
                            }
                        });

                    ui.horizontal(|ui| {
                        if ui.button("Refresh Displays").clicked() {
                            match capture::discover_monitors() {
                                Ok(monitors) => {
                                    self.state.set_monitors(monitors);
                                    if let Err(err) = self.state.ensure_valid_selected_monitor() {
                                        self.show_toast(format!("Monitor refresh failed: {err}"));
                                    }
                                }
                                Err(err) => {
                                    self.show_toast(format!("Monitor refresh failed: {err}"));
                                }
                            }
                        }
                    });

                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Ctrl+Alt+Del stays out of scope, and remote input is intentionally locked out when ROV is running elevated.",
                        )
                        .color(Color32::from_rgb(148, 163, 184)),
                    );
                });

                left.add_space(12.0);
                left.group(|ui| {
                    self.render_security_panel(ui);
                });

                right.group(|ui| {
                    self.render_remote_access_panel(ui, &ctx);
                });

                right.add_space(12.0);
                right.group(|ui| {
                    ui.heading("Connection URLs");
                    ui.label(
                        RichText::new("Best available URL")
                            .color(Color32::from_rgb(148, 163, 184))
                            .strong(),
                    );
                    render_url_row(ui, &ctx, &mut self.toast_message, &preferred_url);

                    if let Some(url) = mobile_url.as_ref().filter(|candidate| {
                        candidate.url != preferred_url.url
                    }) {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Phone URL")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        render_url_row(ui, &ctx, &mut self.toast_message, url);
                    }

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Same machine")
                            .color(Color32::from_rgb(148, 163, 184))
                            .strong(),
                    );
                    render_url_row(ui, &ctx, &mut self.toast_message, &loopback_url);

                    ui.horizontal(|ui| {
                        if ui.button("Refresh Network").clicked() {
                            self.refresh_urls();
                        }

                        if ui.button("Copy Best Available URL").clicked() {
                            self.copy_text(&ctx, "Best available URL", preferred_url.url.clone());
                        }
                    });
                });

                right.add_space(12.0);
                right.group(|ui| {
                    ui.heading("How to Use It");
                    ui.label("1. Start Tailscale on the Windows laptop and on the phone.");
                    ui.label("2. Click Enable HTTPS for iPhone once so Tailscale Serve publishes the phone URL.");
                    ui.label("3. Copy the phone URL above and open it in Safari.");
                    ui.label("4. On the Windows app, generate a one-time pairing code and type it on the phone page.");
                    ui.label("5. Keep remote input off for view-only access, or enable only the control scopes you need.");
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

fn remote_access_copy(status: &TailscaleStatusSnapshot) -> (&'static str, &'static str, Color32) {
    match status.remote_access_mode() {
        RemoteAccessMode::ReadyHttps => (
            "Ready for phone pairing",
            "Trusted HTTPS is already available through Tailscale Serve. The phone still needs a fresh one-time pairing code from this Windows app.",
            Color32::from_rgb(74, 222, 128),
        ),
        RemoteAccessMode::NeedsServe => (
            "Tailscale is ready",
            "MagicDNS is up, but Tailscale Serve has not published the trusted phone URL yet.",
            Color32::from_rgb(245, 158, 11),
        ),
        RemoteAccessMode::NeedsTailscaleLogin => (
            "Tailscale needs attention",
            "Off-LAN access is blocked until this Windows laptop is signed into Tailscale and connected.",
            Color32::from_rgb(245, 158, 11),
        ),
        RemoteAccessMode::NeedsTailscaleInstall => (
            "Off-LAN access not configured",
            "Install Tailscale on the laptop and phone, then sign both devices into the same tailnet.",
            Color32::from_rgb(248, 113, 113),
        ),
        RemoteAccessMode::LocalOnly => (
            "Still local-only",
            "ROV itself is listening only on loopback. Enable Tailscale Serve when you want to publish the trusted phone URL.",
            Color32::from_rgb(245, 158, 11),
        ),
    }
}

fn format_duration_compact(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
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
