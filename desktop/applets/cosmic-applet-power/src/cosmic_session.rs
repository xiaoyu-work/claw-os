// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use zbus::proxy;

#[proxy(
    interface = "com.clawos.Session",
    default_service = "com.clawos.Session",
    default_path = "/com/clawos/Session"
)]
pub trait CosmicSession {
    fn exit(&self) -> zbus::Result<()>;
}
