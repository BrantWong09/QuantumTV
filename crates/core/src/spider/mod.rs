use std::path::{Path, PathBuf};
use std::time::Duration;

const SPIDER_RUNNER_SOURCE: &str = include_str!("SpiderRunner.java");

pub fn calculate_md5(data: &[u8]) -> String {
    format!("{:x}", md5::compute(data))
}

pub fn parse_spider_spec(spec: &str) -> (String, Option<String>) {
    let mut parts = spec.splitn(3, ';');
    let url = parts.next().unwrap_or("").trim().to_string();
    let md5 = if parts.next() == Some("md5") {
        parts.next().map(|s| s.trim().to_string())
    } else {
        None
    };
    (url, md5)
}

#[derive(Debug)]
pub struct SpiderJarHandle {
    pub path: PathBuf,
}

async fn fetch_remote_once(url: &str, timeout_ms: u64) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(5))
        .no_proxy()
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Accept", "*/*".parse().unwrap());
    headers.insert("Accept-Encoding", "identity".parse().unwrap());
    headers.insert("Cache-Control", "no-cache".parse().unwrap());
    headers.insert("Connection", "close".parse().unwrap());

    let ua = if url.contains("github") || url.contains("raw.githubusercontent") {
        "curl/7.68.0"
    } else if url.contains("gitee") || url.contains("gitcode") {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"
    } else if url.contains("jsdelivr") || url.contains("fastly") {
        "DecoTV/1.0"
    } else {
        "Mozilla/5.0 (Linux; Android 11; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Mobile Safari/537.36"
    };
    headers.insert("User-Agent", ua.parse().unwrap());

    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "HTTP {}: {}",
            response.status(),
            response.status().canonical_reason().unwrap_or("Unknown")
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?
        .to_vec();

    if bytes.len() < 1000 {
        return Err(format!("File too small: {} bytes", bytes.len()));
    }
    if bytes[0] != 0x50 || bytes[1] != 0x4B {
        return Err("Invalid JAR file format (missing PK header)".to_string());
    }

    Ok(bytes)
}

async fn fetch_remote(url: &str, timeout_ms: u64, retry_count: usize) -> Option<Vec<u8>> {
    for attempt in 0..=retry_count {
        match fetch_remote_once(url, timeout_ms).await {
            Ok(data) => return Some(data),
            Err(_) => {
                if attempt < retry_count {
                    tokio::time::sleep(Duration::from_secs((attempt + 1) as u64)).await;
                }
            }
        }
    }
    None
}

pub async fn ensure_site_spider_jar(
    site_key: &str,
    spec: &str,
    cache_root: &Path,
) -> Result<SpiderJarHandle, String> {
    let (url, expected_md5) = parse_spider_spec(spec);
    if url.is_empty() {
        return Err(format!("站点 {} 的 spider 配置为空", site_key));
    }

    let cache_name = expected_md5
        .clone()
        .unwrap_or_else(|| format!("{:x}", md5::compute(url.as_bytes())));
    let dir = cache_root
        .join(".cache")
        .join("sites")
        .join(&cache_name);
    let jar_path = dir.join("spider.jar");

    if jar_path.exists() {
        let data = std::fs::read(&jar_path).map_err(|e| format!("读取缓存 jar 失败: {}", e))?;
        if let Some(exp) = &expected_md5 {
            if calculate_md5(&data) == *exp {
                return Ok(SpiderJarHandle { path: jar_path });
            }
        } else {
            return Ok(SpiderJarHandle { path: jar_path });
        }
    }

    let data = fetch_remote(&url, 8000, 1)
        .await
        .ok_or_else(|| format!("下载 spider jar 失败: {}", url))?;

    if let Some(exp) = &expected_md5 {
        let actual = calculate_md5(&data);
        if &actual != exp {
            return Err(format!("MD5 不匹配: 期望 {} 实际 {}", exp, actual));
        }
    }

    std::fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录失败: {}", e))?;
    std::fs::write(&jar_path, &data).map_err(|e| format!("写入 jar 失败: {}", e))?;
    Ok(SpiderJarHandle { path: jar_path })
}

pub fn check_java_available() -> bool {
    std::process::Command::new("java")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn classpath_separator() -> &'static str {
    if cfg!(windows) { ";" } else { ":" }
}

pub fn build_java_args(
    jar_path: &Path,
    runner_dir: &Path,
    action: &str,
    class_name: &str,
    arg: &str,
) -> Vec<String> {
    let sep = classpath_separator();
    let classpath = format!("{}{}{}", jar_path.display(), sep, runner_dir.display());
    vec![
        "-Dfile.encoding=UTF-8".to_string(),
        "-cp".to_string(),
        classpath,
        "SpiderRunner".to_string(),
        jar_path.to_string_lossy().to_string(),
        action.to_string(),
        class_name.to_string(),
        arg.to_string(),
    ]
}

/// 编译 SpiderRunner.java(首次),返回 runner 类目录
pub async fn ensure_runner_compiled(cache_root: &Path) -> Result<PathBuf, String> {
    let runner_dir = cache_root.join(".cache").join("runner");
    let class_file = runner_dir.join("SpiderRunner.class");
    if class_file.exists() {
        return Ok(runner_dir);
    }

    tokio::fs::create_dir_all(&runner_dir)
        .await
        .map_err(|e| format!("创建 runner 目录失败: {}", e))?;

    let src_path = runner_dir.join("SpiderRunner.java");
    tokio::fs::write(&src_path, SPIDER_RUNNER_SOURCE)
        .await
        .map_err(|e| format!("写入 runner 源码失败: {}", e))?;

    let output = tokio::process::Command::new("javac")
        .arg("-encoding")
        .arg("UTF-8")
        .arg("--release")
        .arg("17")
        .arg("-d")
        .arg(&runner_dir)
        .arg(&src_path)
        .output()
        .await
        .map_err(|e| format!("执行 javac 失败: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("编译 SpiderRunner 失败: {}", stderr.trim()));
    }

    Ok(runner_dir)
}

/// 执行 java 子进程并解析 JSON 输出
async fn run_java(
    jar_path: &Path,
    runner_dir: &Path,
    action: &str,
    class_name: &str,
    arg: &str,
) -> Result<String, String> {
    if !check_java_available() {
        return Err("Java 运行时不可用".to_string());
    }

    let args = build_java_args(jar_path, runner_dir, action, class_name, arg);
    let output = tokio::process::Command::new("java")
        .args(&args)
        .output()
        .await
        .map_err(|e| format!("执行 java 失败: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("java 执行失败: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SpiderSearchItem {
    pub vod_id: serde_json::Value,
    pub vod_name: String,
    pub vod_pic: String,
    #[serde(default)]
    pub vod_remarks: Option<String>,
    #[serde(default)]
    pub vod_play_url: Option<String>,
    #[serde(default)]
    pub vod_year: Option<String>,
    #[serde(default)]
    pub vod_content: Option<String>,
    #[serde(default)]
    pub vod_class: Option<String>,
    #[serde(default)]
    pub vod_douban_id: Option<serde_json::Value>,
    #[serde(default)]
    pub type_name: Option<String>,
}

#[derive(serde::Deserialize)]
struct SpiderSearchResponse {
    list: Vec<SpiderSearchItem>,
}

/// Spider 搜索: 下载 JAR → 编译 Runner → Java 子进程 → 解析结果
pub async fn spider_search(
    site_key: &str,
    query: &str,
    class_name: &str,
    spider_spec: &str,
    cache_root: &Path,
) -> Result<Vec<SpiderSearchItem>, String> {
    let handle = ensure_site_spider_jar(site_key, spider_spec, cache_root).await?;
    let runner_dir = ensure_runner_compiled(cache_root).await?;
    let json = run_java(&handle.path, &runner_dir, "search", class_name, query).await?;
    let parsed: SpiderSearchResponse =
        serde_json::from_str(&json)
            .map_err(|e| format!("解析搜索结果失败: {}, body: {}", e, &json[..json.len().min(200)]))?;
    Ok(parsed.list)
}

/// Spider 详情: 流程同搜索,调用 detailContent 方法
pub async fn spider_detail(
    site_key: &str,
    video_id: &str,
    class_name: &str,
    spider_spec: &str,
    cache_root: &Path,
) -> Result<SpiderSearchItem, String> {
    let handle = ensure_site_spider_jar(site_key, spider_spec, cache_root).await?;
    let runner_dir = ensure_runner_compiled(cache_root).await?;
    let json = run_java(&handle.path, &runner_dir, "detail", class_name, video_id).await?;
    let parsed: SpiderSearchResponse =
        serde_json::from_str(&json)
            .map_err(|e| format!("解析详情结果失败: {}, body: {}", e, &json[..json.len().min(200)]))?;
    let mut items = parsed.list;
    items.pop().ok_or_else(|| "详情返回空".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spider_spec_with_md5() {
        let (url, md5) = parse_spider_spec("https://a.jar;md5;abc123");
        assert_eq!(url, "https://a.jar");
        assert_eq!(md5.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_spider_spec_without_md5() {
        let (url, md5) = parse_spider_spec("https://a.jar");
        assert_eq!(url, "https://a.jar");
        assert_eq!(md5, None);
    }

    #[test]
    fn parse_spider_spec_empty() {
        let (url, md5) = parse_spider_spec("");
        assert_eq!(url, "");
        assert_eq!(md5, None);
    }

    #[test]
    fn calculate_md5_known_vector() {
        assert_eq!(calculate_md5(b"hello"), "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn empty_spec_returns_err() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tmp = std::env::temp_dir().join("qtw_spider_test_empty");
        let result = rt.block_on(ensure_site_spider_jar("site", "", &tmp));
        assert!(result.is_err());
    }

    #[test]
    fn disk_cache_hit_returns_existing_jar() {
        let jar_bytes = b"PK\x03\x04valid-jar-content";
        let known_md5 = "a3262411aaf2f68634eff02b21ea157f";
        let rt = tokio::runtime::Runtime::new().unwrap();

        let cache_root = std::env::temp_dir().join("qtw_spider_test_cache_hit");
        let _ = std::fs::remove_dir_all(&cache_root);
        let cache_dir = cache_root.join(".cache").join("sites").join(known_md5);
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join("spider.jar"), jar_bytes).unwrap();

        let spec = format!("https://example.com/s.jar;md5;{}", known_md5);
        let result = rt.block_on(ensure_site_spider_jar("site", &spec, &cache_root));
        assert!(result.is_ok(), "缓存命中应返回 Ok,stub 返回 Err");
    }

    #[test]
    fn download_and_cache_jar_from_url() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let mut jar_bytes: Vec<u8> = b"PK\x03\x04".to_vec();
        jar_bytes.extend(std::iter::repeat(b'A').take(1100));
        let known_md5 = "b3dc889f6447ad766faf659132180c80";
        let rt = tokio::runtime::Runtime::new().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let jar = jar_bytes.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                jar.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&jar);
        });

        let cache_root = std::env::temp_dir().join("qtw_spider_test_download");
        let _ = std::fs::remove_dir_all(&cache_root);
        let spec = format!("http://{}/s.jar;md5;{}", addr, known_md5);

        let result = rt.block_on(ensure_site_spider_jar("site", &spec, &cache_root));
        assert!(result.is_ok(), "下载成功应返回 Ok,当前未实现下载");
        if let Ok(handle) = result {
            let cached = std::fs::read(&handle.path).unwrap();
            assert_eq!(cached, jar_bytes);
        }
    }

    #[test]
    fn build_java_args_on_windows() {
        let jar = Path::new("j.jar");
        let runner = Path::new("r");
        let args = build_java_args(jar, runner, "search", "MyClass", "keyword");
        // 第 4 个参数(0-indexed)是 SpiderRunner,第 5 个是 jar_path
        // 顺序: -Dfile.encoding=UTF-8, -cp, classpath, SpiderRunner, jar_path, action, class_name, arg
        assert_eq!(args[3], "SpiderRunner");
        assert_eq!(args[4], "j.jar");
        assert_eq!(args[5], "search");
        assert_eq!(args[6], "MyClass");
        assert_eq!(args[7], "keyword");
    }
}