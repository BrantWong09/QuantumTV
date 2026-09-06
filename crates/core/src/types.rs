use serde::{Deserialize, Serialize};

/// 单个站点详情里的一组源头 (线路), 对应 vod_play_from 中一个 flag 及其剧集
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct PlayGroup {
    /// 源头名 (如 百度网盘 / 夸克网盘 / UC网盘)
    #[serde(default)]
    pub flag: String,
    #[serde(default)]
    pub episodes: Vec<String>,
    #[serde(default)]
    pub episodes_titles: Vec<String>,
    /// Spider 网盘集原始 id (与 episodes 对齐; 非网盘源为空)
    #[serde(default)]
    pub episodes_raw: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SearchResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub poster: String,
    #[serde(default)]
    pub episodes: Vec<String>,
    #[serde(default)]
    pub episodes_titles: Vec<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_name: String,
    pub class: Option<String>,
    pub year: Option<String>,
    pub desc: Option<String>,
    pub type_name: Option<String>,
    pub douban_id: Option<i32>,
    /// 站点类型 (1=CMS, 3=Spider); 供前端区分 spider 结果
    #[serde(default)]
    pub source_site_type: Option<i32>,
    /// 播放受阻的用户提示 (如"该源为网盘资源, 需要登录网盘账号")
    #[serde(default)]
    pub login_hint: Option<String>,
    /// Spider 网盘集原始 id (直链化前的待解析列表, 与 episodes 对齐; 已解析集为空)
    #[serde(default)]
    pub episodes_raw: Vec<String>,
    /// 该详情内部的多组源头 (线路), 每个含独立剧集; 顶层 episodes 为当前默认组
    #[serde(default)]
    pub play_groups: Vec<PlayGroup>,
}
