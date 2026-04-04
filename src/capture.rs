use anyhow::{Context, Result, anyhow};
use image::{DynamicImage, codecs::jpeg::JpegEncoder, imageops::FilterType};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime},
};
use xcap::Monitor;

use crate::{
    model::{LatestFrame, MonitorInfo},
    state::{AppState, preferred_monitor},
};

#[derive(Clone)]
struct DiscoveredMonitor {
    info: MonitorInfo,
    handle: Monitor,
}

pub fn discover_monitors() -> Result<Vec<MonitorInfo>> {
    Ok(discover_monitor_handles()?
        .into_iter()
        .map(|monitor| monitor.info)
        .collect())
}

pub fn spawn_capture_worker(state: Arc<AppState>) {
    thread::spawn(move || capture_loop(state));
}

fn capture_loop(state: Arc<AppState>) {
    let mut current_monitor: Option<Monitor> = None;
    let mut current_monitor_id = None;
    let mut last_inventory_refresh = Instant::now() - Duration::from_secs(30);

    loop {
        let requested_monitor_id = state.selected_monitor_id();
        let needs_refresh = current_monitor.is_none()
            || current_monitor_id != requested_monitor_id
            || last_inventory_refresh.elapsed() >= Duration::from_secs(5);

        if needs_refresh {
            match refresh_monitor_inventory(&state, requested_monitor_id) {
                Ok((handle, active_monitor)) => {
                    current_monitor = Some(handle);
                    current_monitor_id = Some(active_monitor.id);
                    last_inventory_refresh = Instant::now();

                    if Some(active_monitor.id) != requested_monitor_id
                        && let Err(err) = state.set_selected_monitor(active_monitor.id)
                    {
                        tracing::warn!(error = %err, "Failed to update selected monitor");
                    }
                }
                Err(err) => {
                    state.set_capture_error(err.to_string());
                    current_monitor = None;
                    current_monitor_id = None;
                    thread::sleep(Duration::from_millis(900));
                    continue;
                }
            }
        }

        let Some(monitor) = current_monitor.as_ref() else {
            thread::sleep(Duration::from_millis(500));
            continue;
        };

        let (jpeg_quality, max_frame_width) = state.capture_settings();
        match capture_monitor_frame(monitor, jpeg_quality, max_frame_width) {
            Ok(frame) => state.update_frame(frame),
            Err(err) => {
                state.set_capture_error(err.to_string());
                current_monitor = None;
                current_monitor_id = None;
            }
        }

        thread::sleep(Duration::from_millis(180));
    }
}

fn refresh_monitor_inventory(
    state: &AppState,
    requested_monitor_id: Option<u32>,
) -> Result<(Monitor, MonitorInfo)> {
    let discovered = discover_monitor_handles()?;
    let infos = discovered
        .iter()
        .map(|monitor| monitor.info.clone())
        .collect::<Vec<_>>();
    state.set_monitors(infos.clone());

    let selected = preferred_monitor(requested_monitor_id, &infos)
        .ok_or_else(|| anyhow!("no monitors are available for screen capture"))?;

    let handle = discovered
        .into_iter()
        .find(|monitor| monitor.info.id == selected.id)
        .map(|monitor| monitor.handle)
        .ok_or_else(|| anyhow!("selected monitor disappeared during refresh"))?;

    Ok((handle, selected))
}

fn discover_monitor_handles() -> Result<Vec<DiscoveredMonitor>> {
    let mut monitors = Vec::new();

    for monitor in Monitor::all().context("failed to query desktop monitors")? {
        let id = monitor.id().context("failed to read monitor id")?;
        let friendly_name = monitor.friendly_name().ok();
        let technical_name = monitor.name().ok();
        let name = friendly_name
            .filter(|name| !name.trim().is_empty())
            .or(technical_name.filter(|name| !name.trim().is_empty()))
            .unwrap_or_else(|| format!("Display {id}"));

        monitors.push(DiscoveredMonitor {
            info: MonitorInfo {
                id,
                name,
                x: monitor.x().context("failed to read monitor x position")?,
                y: monitor.y().context("failed to read monitor y position")?,
                width: monitor.width().context("failed to read monitor width")?,
                height: monitor.height().context("failed to read monitor height")?,
                is_primary: monitor.is_primary().unwrap_or(false),
            },
            handle: monitor,
        });
    }

    monitors.sort_by_key(|monitor| {
        (
            !monitor.info.is_primary,
            monitor.info.y,
            monitor.info.x,
            monitor.info.id,
        )
    });

    Ok(monitors)
}

fn capture_monitor_frame(
    monitor: &Monitor,
    jpeg_quality: u8,
    max_frame_width: u32,
) -> Result<LatestFrame> {
    let rgba = monitor
        .capture_image()
        .context("failed to capture the current monitor frame")?;
    let source_width = rgba.width();
    let source_height = rgba.height();
    let image = DynamicImage::ImageRgba8(rgba);

    let prepared = if source_width > max_frame_width {
        let target_height = (((source_height as f32) * (max_frame_width as f32)
            / (source_width as f32))
            .round() as u32)
            .max(1);
        image.resize(max_frame_width, target_height, FilterType::Triangle)
    } else {
        image
    };

    let encoded_width = prepared.width();
    let encoded_height = prepared.height();

    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg, jpeg_quality);
    encoder
        .encode_image(&prepared)
        .context("failed to encode the captured frame as JPEG")?;

    Ok(LatestFrame {
        jpeg: Arc::new(jpeg),
        source_width,
        source_height,
        encoded_width,
        encoded_height,
        captured_at: SystemTime::now(),
    })
}
