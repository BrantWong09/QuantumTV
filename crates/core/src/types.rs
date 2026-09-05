use serde::{Deserialize, Serialize};

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
}
