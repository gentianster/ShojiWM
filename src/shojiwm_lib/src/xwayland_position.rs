//! Ordered native surface positions for the embedded Xwayland bridge.
//!
//! A numeric Wayland id is reusable. Keep the exact resource and its tracking
//! object instead, and seal geometry on the same connection before native input.

use satellite::position_protocol::server::{
    shoji_xwayland_position_manager_v1::{self, ShojiXwaylandPositionManagerV1},
    shoji_xwayland_position_v1::{self, ShojiXwaylandPositionV1},
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    protocol::wl_surface::WlSurface,
};
use smithay::utils::IsAlive;
use smithay::wayland::seat::WaylandFocus;

use crate::state::{ClientState, ShojiWM};

#[derive(Clone, Copy, Debug, PartialEq)]
struct SurfacePosition {
    x: f64,
    y: f64,
    geometry_x: i32,
    geometry_y: i32,
}

struct TrackedSurface {
    resource: ShojiXwaylandPositionV1,
    manager: ShojiXwaylandPositionManagerV1,
    surface: WlSurface,
    // Outer None forces an initial state or invalidate event.
    last: Option<Option<SurfacePosition>>,
    destroyed: bool,
}

#[derive(Default)]
pub struct XwaylandPositionState {
    tracked: Vec<TrackedSurface>,
    epoch: u32,
}

impl XwaylandPositionState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<ShojiWM, ShojiXwaylandPositionManagerV1, _>(1, ());
        Self::default()
    }
}

impl ShojiWM {
    fn xwayland_surface_position(&self, surface: &WlSurface) -> Option<SurfacePosition> {
        if !surface.is_alive() {
            return None;
        }
        self.space.elements().find_map(|window| {
            if !window.alive() || window.wl_surface().as_deref() != Some(surface) {
                return None;
            }
            let location = self.space.element_location(window)?;
            let geometry = window.geometry();
            // Match the custom renderer's client-slot selection. Space's
            // generic geometry origin alone does not describe SSD placement.
            let (client_x, client_y) = self
                .window_decorations
                .get(window)
                .and_then(|decoration| {
                    decoration.content_clip.map(|clip| {
                        (
                            f64::from(clip.rect_precise.x) + decoration.root_subpixel_offset.left,
                            f64::from(clip.rect_precise.y) + decoration.root_subpixel_offset.top,
                        )
                    })
                })
                .unwrap_or((f64::from(location.x), f64::from(location.y)));
            Some(SurfacePosition {
                x: client_x - f64::from(geometry.loc.x),
                y: client_y - f64::from(geometry.loc.y),
                geometry_x: geometry.loc.x,
                geometry_y: geometry.loc.y,
            })
        })
    }

    /// Queue changed origins after output membership and before native input.
    /// Unchanged pointer motion does not trigger Space maintenance or traffic.
    pub(crate) fn publish_xwayland_positions(&mut self) {
        self.xwayland_position_state
            .tracked
            .retain(|tracked| tracked.resource.is_alive() && tracked.manager.is_alive());
        let updates = self
            .xwayland_position_state
            .tracked
            .iter()
            .enumerate()
            .filter_map(|(index, tracked)| {
                let position = if tracked.destroyed {
                    None
                } else {
                    self.xwayland_surface_position(&tracked.surface)
                };
                (tracked.last != Some(position)).then_some((index, position))
            })
            .collect::<Vec<_>>();
        if updates.is_empty() {
            return;
        }

        // Enter/leave events must precede the position epoch that uses the
        // new layout. Refresh only for actual changes, not every mouse move.
        self.space.refresh();
        let state = &mut self.xwayland_position_state;
        state.epoch = state.epoch.wrapping_add(1);
        let epoch = state.epoch;
        let mut managers = Vec::new();
        for (index, position) in updates {
            let tracked = &mut state.tracked[index];
            match position {
                Some(position) => tracked.resource.state(
                    epoch,
                    position.x,
                    position.y,
                    position.geometry_x,
                    position.geometry_y,
                ),
                None => tracked.resource.invalidate(epoch),
            }
            tracked.last = Some(position);
            if !managers.contains(&tracked.manager) {
                managers.push(tracked.manager.clone());
            }
        }
        for manager in managers {
            manager.done(epoch);
        }
    }

    pub(crate) fn invalidate_xwayland_surface_position(&mut self, surface: &WlSurface) {
        for tracked in &mut self.xwayland_position_state.tracked {
            if tracked.surface == *surface {
                tracked.destroyed = true;
            }
        }
        self.publish_xwayland_positions();
    }
}

impl GlobalDispatch<ShojiXwaylandPositionManagerV1, ()> for ShojiWM {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<ShojiXwaylandPositionManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(manager, ());
    }

    fn can_view(client: Client, _data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .is_some_and(|data| data.xwayland_position_allowed)
    }
}

impl Dispatch<ShojiXwaylandPositionManagerV1, ()> for ShojiWM {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ShojiXwaylandPositionManagerV1,
        request: shoji_xwayland_position_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            shoji_xwayland_position_manager_v1::Request::GetSurface { id, surface } => {
                let resource = data_init.init(id, ());
                state.xwayland_position_state.tracked.push(TrackedSurface {
                    resource,
                    manager: manager.clone(),
                    surface,
                    last: None,
                    destroyed: false,
                });
                state.publish_xwayland_positions();
            }
            shoji_xwayland_position_manager_v1::Request::Destroy => {
                state
                    .xwayland_position_state
                    .tracked
                    .retain(|tracked| tracked.manager != *manager);
            }
            _ => {}
        }
    }
}

impl Dispatch<ShojiXwaylandPositionV1, ()> for ShojiWM {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ShojiXwaylandPositionV1,
        request: shoji_xwayland_position_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let shoji_xwayland_position_v1::Request::Destroy = request {
            state
                .xwayland_position_state
                .tracked
                .retain(|tracked| tracked.resource != *resource);
        }
    }
}
