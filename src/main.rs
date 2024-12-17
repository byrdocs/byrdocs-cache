use clap::{Parser, ValueEnum};
use colored::*;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use std::{collections::HashMap, process::exit};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(value_enum, default_value_t = CheckType::All)]
    check_type: CheckType,

    #[arg(short, long, default_value = None)]
    cookie: Option<String>
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum, Debug)]
enum CheckType {
    Webp,
    Jpg,
    File,
    All,
}

#[derive(Deserialize, Debug)]
struct FileMetadata {
    id: String,
    data: FileData,
}

#[derive(Deserialize, Debug)]
struct FileData {
    filetype: String,
}

#[derive(Debug, Default)]
struct CacheStats {
    total: u32,
    hit: u32,
    miss: u32,
    unknown: u32,
    total_age: u64,
}

fn format_duration(seconds: u64) -> String {
    if seconds == 0 {
        return "0s".to_string();
    }

    let days = seconds / (24 * 3600);
    let hours = (seconds % (24 * 3600)) / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{}d", days));
    }
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if secs > 0 {
        parts.push(format!("{}s", secs));
    }

    parts.truncate(2);
    parts.join("")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    
    println!("{}", "正在获取文件元数据...".blue());
    let metadata: Vec<FileMetadata> = reqwest::get("https://files.byrdocs.org/metadata2.json")
        .await?
        .json()
        .await?;

    println!("{}", "元数据获取完成".green());
    println!("{}", "计算文件列表...".blue());

    let mut files_to_check = Vec::new();
    for file in &metadata {
        match cli.check_type {
            CheckType::File => {
                files_to_check.push(format!("{}.{}", file.id, file.data.filetype));
            }
            CheckType::Jpg => {
                if file.data.filetype == "pdf" {
                    files_to_check.push(format!("{}.jpg", file.id));
                }
            }
            CheckType::Webp => {
                if file.data.filetype == "pdf" {
                    files_to_check.push(format!("{}.webp", file.id));
                }
            }
            CheckType::All => {
                files_to_check.push(format!("{}.{}", file.id, file.data.filetype));
                if file.data.filetype == "pdf" {
                    files_to_check.push(format!("{}.jpg", file.id));
                    files_to_check.push(format!("{}.webp", file.id));
                }
            }
        }
    }

    println!("{}", format!("文件列表计算完成，共 {} 个文件", files_to_check.len()).green());

    let pb = ProgressBar::new(files_to_check.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise} / {eta}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)")
            .unwrap()
            .progress_chars("#>-"),
    );

    let client = reqwest::Client::new();
    let mut stats = CacheStats::default();
    let mut results = HashMap::new();

    let tasks = futures::stream::iter(
        files_to_check.iter().map(|filename| {
            let client = client.clone();
            let filename = filename.clone();
            let pbc = pb.clone();
            let cookie = cli.cookie.clone();
            async move {
                let url = format!("https://byrdocs.org/files/{}", filename);
                let resp = client
                    .head(&url)
                    .header("cookie", cookie.as_deref().unwrap_or(""))
                    .send().await;

                if let Ok(r) = &resp {
                    match r.headers().get("content-type") {
                        Some(v) => {
                            if v.to_str().unwrap_or("").starts_with("text/html") {
                                match client.get(&url).send().await {
                                    Ok(resp) => {
                                        let content = resp.text().await.unwrap();
                                        if content.contains("您没有使用北邮校园网(IPv6)访问本站") {
                                            println!("{}", "error: 未登录, 使用 --cookie 参数登录".red());
                                            exit(1)
                                        }
                                        let filename = format!("{}.html", filename);
                                        std::fs::write(&filename, content).unwrap();
                                        println!("error: {}", filename);
                                        println!("\tfile saved as {}", filename);
                                    },
                                    Err(e) => {
                                        println!("error to get {}: {}", url, e);
                                    }
                                }
                                panic!();
                            }
                        },
                        _ => {
                            println!("not found content-type: {}", url);
                            panic!();
                        }
                    };
                }

                pbc.inc(1);
                (filename, resp)
            }
        })
    )
    .buffer_unordered(20)
    .collect::<Vec<_>>()
    .await;

    for (filename, resp) in tasks {
        stats.total += 1;
        
        let status = match resp {
            Ok(resp) => {
                match resp.headers().get("cf-cache-status").map(|v| v.to_str().unwrap_or("")) {
                    Some("HIT") => {
                        stats.hit += 1;
                        if let Some(age) = resp.headers().get("age") {
                            if let Ok(age_secs) = age.to_str().unwrap_or("0").parse::<u64>() {
                                stats.total_age += age_secs;
                                format!("HIT (age: {})", format_duration(age_secs)).green()
                            } else {
                                "HIT".green()
                            }
                        } else {
                            "HIT".green()
                        }
                    },
                    Some("MISS") => {
                        stats.miss += 1;
                        "MISS".red()
                    },
                    Some(state) => {
                        stats.unknown += 1;
                        format!("UNKNOWN: {}", state).yellow()
                    },
                    None => {
                        stats.unknown += 1;
                        "None".yellow()
                    }
                }
            },
            Err(e) => {
                stats.unknown += 1;
                format!("ERROR: {}", e).yellow()
            }
        };

        results.insert(filename, status);
    }

    pb.finish_with_message("检查完成");

    println!("\n{}", "详细结果:".blue().bold());
    for (filename, status) in results {
        println!("  {}: {}", filename, status);
    }

    println!("\n{}", "统计信息:".blue().bold());
    println!("总文件数: {}", stats.total);
    println!("已缓存: {}", stats.hit);
    println!("未缓存: {}", stats.miss);
    println!("未知状态: {}", stats.unknown);
    
    if stats.total > 0 {
        let cache_ratio = (stats.hit as f64 / stats.total as f64 * 100.0).round();
        println!("缓存率: {}%", cache_ratio);
        if stats.hit > 0 {
            let avg_age = stats.total_age / stats.hit as u64;
            println!("平均缓存时间: {}", format_duration(avg_age));
        }
    }

    Ok(())
}