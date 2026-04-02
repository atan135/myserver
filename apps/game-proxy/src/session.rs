#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxySessionState {
    Connected,
    SelectingUpstream,
    Proxying,
    Draining,
    Closed,
}

#[derive(Debug)]
pub struct ProxySession {
    pub id: u64,
    pub state: ProxySessionState,
    pub upstream_server_id: Option<String>,
}

impl ProxySession {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            state: ProxySessionState::Connected,
            upstream_server_id: None,
        }
    }
}
