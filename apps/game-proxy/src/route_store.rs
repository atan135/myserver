use std::sync::Arc;

use tokio::sync::RwLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpstreamState {
    Active,
    Draining,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct UpstreamRoute {
    pub server_id: String,
    pub local_socket_name: String,
    pub state: UpstreamState,
}

#[derive(Clone, Default)]
pub struct ProxyRouteStore {
    routes: Arc<RwLock<Vec<UpstreamRoute>>>,
}

impl ProxyRouteStore {
    pub async fn set_routes(&self, routes: Vec<UpstreamRoute>) {
        *self.routes.write().await = routes;
    }

    pub async fn list_routes(&self) -> Vec<UpstreamRoute> {
        self.routes.read().await.clone()
    }

    pub async fn select_active(&self) -> Option<UpstreamRoute> {
        self.routes
            .read()
            .await
            .iter()
            .find(|route| route.state == UpstreamState::Active)
            .cloned()
    }

    pub async fn update_state(&self, server_id: &str, state: UpstreamState) -> bool {
        let mut routes = self.routes.write().await;
        let mut updated = false;
        for route in routes.iter_mut() {
            if route.server_id == server_id {
                route.state = state;
                updated = true;
            }
        }
        updated
    }
}
