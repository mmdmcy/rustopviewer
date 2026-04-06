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
                    "Trusted Tailscale HTTPS is ready. Open the new HTTPS phone URL in Safari.",
                );
            }
            Err(err) => {
                self.show_toast(format!("Tailscale HTTPS setup failed: {err}"));
            }
        }
    }

    fn render_mobile_access_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let status = self.urls.tailscale_status.clone();
        let mobile_url = self.urls.mobile_data_preferred.clone();
        let https_url = self.urls.tailscale_https.clone();
        let serve_command = format!("tailscale serve --bg --yes {}", self.state.port());
        let (headline, detail, accent) = remote_access_copy(&status);

        ui.heading("Away From Home");
        ui.label(
            RichText::new("Use this when the phone is on mobile data or a different Wi-Fi.")
                .color(Color32::from_rgb(148, 163, 184)),
        );
        ui.add_space(6.0);
        ui.label(RichText::new(headline).color(accent).strong());
        ui.label(RichText::new(detail).color(Color32::from_rgb(226, 232, 240)));

        if let Some(url) = mobile_url.as_ref() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Best phone URL")
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
                RichText::new("Trusted HTTPS URL")
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

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Copy Phone URL").clicked() {
                if let Some(url) = mobile_url.as_ref() {
                    self.copy_text(ctx, "Phone URL", url.url.clone());
                } else {
                    self.show_toast(
                        "No off-LAN phone URL is available yet. Start Tailscale first.",
                    );
                }
            }

            let can_enable_https = status.is_running && status.magic_dns_enabled;
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
                        "Safari can open the HTTPS link directly while the phone stays on mobile data.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::ReadyTailscale => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Off-LAN access already works over Tailscale. The HTTPS button above just removes the browser warning in Safari.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::NeedsTailscaleLogin => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Open Tailscale on the Windows laptop and the phone, then sign them into the same tailnet before trying the remote URL.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::NeedsTailscaleInstall => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "This project uses Tailscale as the safe off-LAN path. Without it, the app stays limited to the local network.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
            RemoteAccessMode::LanOnly => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Tailscale is installed, but the laptop has not published a usable tailnet name or IP yet.",
                    )
                    .small()
                    .color(Color32::from_rgb(100, 116, 139)),
                );
            }
        }

        if status.is_running && !status.serve_enabled {
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
}

impl App for RustOpViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.maybe_refresh_urls();
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(400));

        let preferred_url = self.urls.preferred.clone();
        let mobile_url = self.urls.mobile_data_preferred.clone();
        let tailscale_dns = self.urls.tailscale_dns.clone();
        let tailscale_https = self.urls.tailscale_https.clone();
        let tailscale_urls = self.urls.tailscale.clone();
        let lan_urls = self.urls.lan.clone();
        let loopback_url = self.urls.loopback.clone();
        let tailscale_status = self.urls.tailscale_status.clone();

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading(
                    RichText::new("RustOp Viewer")
                        .size(30.0)
                        .color(Color32::from_rgb(241, 245, 249)),
                );
                ui.label(
                    RichText::new(
                        "Remote desktop viewing and full mouse/keyboard control for Windows 11 over Tailscale, including when the phone is on mobile data.",
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
                        RichText::new(
                            "When Tailscale is active, the phone can keep using ROV even away from the laptop's Wi-Fi.",
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

                        if ui.button("Regenerate Secure Link").clicked() {
                            match self.state.regenerate_auth_token() {
                                Ok(_) => {
                                    self.refresh_urls();
                                    self.show_toast(
                                        "Phone link rotated; old links now stop working",
                                    );
                                }
                                Err(err) => {
                                    self.show_toast(format!("Token rotation failed: {err}"));
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
                    self.render_mobile_access_panel(ui, &ctx);
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
                            RichText::new("Best phone URL")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        render_url_row(ui, &ctx, &mut self.toast_message, url);
                    }

                    if let Some(url) = tailscale_https.as_ref().filter(|candidate| {
                        candidate.url != preferred_url.url
                            && mobile_url
                                .as_ref()
                                .is_none_or(|selected| selected.url != candidate.url)
                    }) {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Trusted HTTPS")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        render_url_row(ui, &ctx, &mut self.toast_message, url);
                    }

                    if let Some(url) = tailscale_dns.as_ref().filter(|candidate| {
                        candidate.url != preferred_url.url
                            && mobile_url
                                .as_ref()
                                .is_none_or(|selected| selected.url != candidate.url)
                    }) {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("MagicDNS over Tailscale")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        render_url_row(ui, &ctx, &mut self.toast_message, url);
                    }

                    if !tailscale_urls.is_empty() {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Tailscale IP URLs")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        for url in &tailscale_urls {
                            render_url_row(ui, &ctx, &mut self.toast_message, url);
                        }
                    }

                    if !lan_urls.is_empty() {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Local network")
                                .color(Color32::from_rgb(148, 163, 184))
                                .strong(),
                        );
                        for url in &lan_urls {
                            render_url_row(ui, &ctx, &mut self.toast_message, url);
                        }
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

                    if tailscale_status.is_running && !tailscale_status.magic_dns_enabled {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "Enable MagicDNS in Tailscale for a stable browser hostname instead of only using 100.x.x.x URLs.",
                            )
                            .small()
                            .color(Color32::from_rgb(100, 116, 139)),
                        );
                    }
                });

                right.add_space(12.0);
                right.group(|ui| {
                    ui.heading("How to Use It");
                    ui.label("1. Start Tailscale on both the Windows laptop and the phone.");
                    ui.label("2. When the phone is on mobile data or another Wi-Fi, copy the Best phone URL above and open it in Safari.");
                    ui.label("3. If Safari shows the page as not secure, click Enable HTTPS for iPhone once on the laptop and then use the HTTPS URL.");
                    ui.label("4. Tap the live image for left click, long-press for right click, and use Drag mode when you need to hold the mouse button down.");
                    ui.label("5. Use the text box and shortcut buttons on the phone page for typing and common Windows commands.");
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
            "Ready for mobile data",
            "Trusted HTTPS is already available through Tailscale Serve on this laptop.",
            Color32::from_rgb(74, 222, 128),
        ),
        RemoteAccessMode::ReadyTailscale => (
            "Ready over Tailscale",
            "The phone can reach this laptop from another network right now. HTTPS is optional but recommended for Safari.",
            Color32::from_rgb(74, 222, 128),
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
        RemoteAccessMode::LanOnly => (
            "Still LAN-only",
            "The app is listening, but no usable Tailscale hostname or IP is available yet for mobile-data access.",
            Color32::from_rgb(245, 158, 11),
        ),
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
