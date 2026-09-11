use crate::state::{ClientState, State};
use smithay::{
    delegate_security_context,
    reexports::wayland_server::backend::DisconnectReason,
    wayland::security_context::{
        SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
    },
};
use std::sync::Arc;
use tracing::warn;

impl SecurityContextHandler for State {
    fn context_created(
        &mut self,
        source: SecurityContextListenerSource,
        security_context: SecurityContext,
    ) {
        let data = match self
            .common
            .display_handle
            .backend_handle()
            .get_client_data(security_context.creator_client_id.clone())
        {
            Ok(data) => data,
            Err(error) => {
                warn!(?error, "Security context creator disappeared");
                return;
            }
        };
        let Some(creator) = data.downcast_ref::<ClientState>() else {
            self.common.display_handle.backend_handle().kill_client(
                security_context.creator_client_id.clone(),
                DisconnectReason::ConnectionClosed,
            );
            warn!("Security context creator has no authenticated display state");
            return;
        };
        let origin = match creator
            .display_origin
            .as_ref()
            .ok_or(claw_display_control::Error::Identity)
            .and_then(|origin| origin.context())
        {
            Ok(origin) => origin,
            Err(error) => {
                self.common.display_handle.backend_handle().kill_client(
                    security_context.creator_client_id.clone(),
                    DisconnectReason::ConnectionClosed,
                );
                warn!(%error, "Security context creator authority refused");
                return;
            }
        };
        let drm_node = creator.advertised_drm_node;
        let creator_id = security_context.creator_client_id.clone();
        let tracked_origin = origin.clone();
        let result = self.common.event_loop_handle.insert_source(
            source,
            move |client_stream, _, state| {
                match origin.accepts(&client_stream) {
                    Ok(true) => {}
                    Ok(false) => {
                        warn!(
                            "Security context rejected a client outside its authenticated instance"
                        );
                        return;
                    }
                    Err(error) => {
                        warn!(%error, "Security context client identity failed");
                        return;
                    }
                }
                let new_state = state.new_client_state();
                match state.common.display_handle.insert_client(
                    client_stream,
                    Arc::new(ClientState {
                        security_context: Some(security_context.clone()),
                        advertised_drm_node: drm_node.or(new_state.advertised_drm_node),
                        display_origin: Some(origin.clone()),
                        ..new_state
                    }),
                ) {
                    Ok(client) => {
                        let result = state
                            .common
                            .display_authority
                            .as_mut()
                            .ok_or(claw_display_control::Error::Identity)
                            .and_then(|authority| authority.track_client(&state.common.display_handle, &origin, client.clone()));
                        if let Err(error) = result {
                            state
                                .common
                                .display_handle
                                .backend_handle()
                                .kill_client(client.id(), DisconnectReason::ConnectionClosed);
                            warn!(%error, "GUI connection tracking failed");
                        }
                    }
                    Err(error) => warn!(?error, "Error adding authenticated Wayland client"),
                }
            },
        );
        match result {
            Ok(token) => {
                let tracking = self
                    .common
                    .display_authority
                    .as_mut()
                    .ok_or(claw_display_control::Error::Identity)
                    .and_then(|authority| authority.track_listener(&tracked_origin, token));
                if let Err(error) = tracking {
                    self.common.event_loop_handle.remove(token);
                    self.common.display_handle.backend_handle().kill_client(creator_id, DisconnectReason::ConnectionClosed);
                    warn!(%error, "GUI listener tracking failed");
                }
            }
            Err(error) => {
                self.common.display_handle.backend_handle().kill_client(creator_id, DisconnectReason::ConnectionClosed);
                warn!(?error, "Failed to install authenticated security context listener");
            }
        }
    }
}
delegate_security_context!(State);
