// SPDX-License-Identifier: GPL-3.0-only
use tokio::sync::mpsc;
use zbus::interface;

pub enum SessionRequest {
	Exit,
	Restart,
}

pub struct SessionService {
	pub session_tx: mpsc::Sender<SessionRequest>,
}

#[interface(name = "com.clawos.Session")]
impl SessionService {
	async fn exit(&mut self) {
		warn!("exiting session");
		_ = self.session_tx.send(SessionRequest::Exit).await;
	}

	async fn restart(&self) {
		warn!("restarting session");
		_ = self.session_tx.send(SessionRequest::Restart).await;
	}
}
