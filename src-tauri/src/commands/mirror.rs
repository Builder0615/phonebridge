//! 镜像元数据（事件负载）。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorMetadataPayload {
    pub width: u32,
    pub height: u32,
    pub rotation: u8,
    pub codec: Option<String>,
}
