//! 業務文書テンプレート集
//!
//! エディタのツールバーから選択して、カーソル位置に定型文書構造を一括挿入するための
//! あらかじめ用意された文書テンプレート。独自拡張構文（表の列幅、配置、強調色）の
//! 実例を兼ねており、ユーザーはゼロから構文を覚えなくても典型的な業務文書を書き始められる。

/// `(メニュー表示名, 挿入する原稿テキスト)` の一覧
pub const TEMPLATES: &[(&str, &str)] = &[
    ("📝 議事録", MEETING_MINUTES),
    ("🚨 障害・インシデント報告書", INCIDENT_REPORT),
    ("📨 稟議書", APPROVAL_REQUEST),
    ("📅 日報", DAILY_REPORT),
];

const MEETING_MINUTES: &str = r#"
{align: right}
2026年10月16日

{align: center, size: 15pt}
**定例会議 議事録**

## 1. 会議概要

{widths: 25%, 75%}
| 項目 | 内容 |
| :--- | :--- |
| 日時 | 2026年10月16日（金）10:00〜11:00 |
| 場所 | 第3会議室 / オンライン |
| 出席者 | 入力してください |
| 欠席者 | 入力してください |

## 2. 議題

1. 議題を入力
2. 議題を入力

## 3. 決定事項

[重要]{red} 決定事項をここに記載します。

## 4. 宿題事項（ToDo）

{widths: 40%, 30%, 30%}
| タスク | 担当 | 期限 |
| :--- | :--- | :--- |
| 入力してください | 担当者 | 10月20日 |

"#;

const INCIDENT_REPORT: &str = r#"
{align: right}
2026年10月16日

{align: center, size: 16pt}
**障害・インシデント報告書**

## 1. 概要

[発生日時]{blue}：2026年10月16日 00:00〜00:00
[影響範囲]{red}：影響範囲をここに記載します。

## 2. 発生状況と対応経緯

{widths: 20%, 55%, 25%}
| 時刻 | 対応内容 | 担当 |
| :--- | :--- | :--- |
| 00:00 | 検知 | 監視チーム |
| 00:00 | 一次対応 | 入力してください |

## 3. 原因

原因をここに記載します。

## 4. 再発防止策

{widths: 15%, 55%, 30%}
| 優先度 | 対策項目 | 完了期限 |
| :--- | :--- | :--- |
| [最優先]{red} | 対策内容を入力 | 10月20日 |

"#;

const APPROVAL_REQUEST: &str = r#"
{align: right}
2026年10月16日

{align: center, size: 15pt}
**稟議書**

{widths: 25%, 75%}
| 項目 | 内容 |
| :--- | :--- |
| 件名 | 入力してください |
| 申請者 | 入力してください |
| 所属部署 | 入力してください |
| 金額 | 入力してください |

## 1. 申請理由

申請理由をここに記載します。

## 2. 内容・詳細

内容・詳細をここに記載します。

## 3. 承認欄

{widths: 33%, 33%, 34%}
| 申請者 | 承認者 | 最終承認者 |
| :---: | :---: | :---: |
|  |  |  |

"#;

const DAILY_REPORT: &str = r#"
{align: right}
2026年10月16日　氏名：

{align: center, size: 15pt}
**日報**

## 本日の業務内容

- 入力してください

## 進捗・成果

[順調]{green} 進捗状況をここに記載します。

## 課題・相談事項

[要確認]{red} 課題があればここに記載します。

## 明日の予定

- 入力してください

"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_document;

    #[test]
    fn test_all_templates_are_registered_and_non_empty() {
        assert_eq!(TEMPLATES.len(), 4);
        for (name, body) in TEMPLATES {
            assert!(!name.is_empty());
            assert!(
                !body.trim().is_empty(),
                "template {name} should not be empty"
            );
        }
    }

    #[test]
    fn test_all_templates_parse_without_panicking() {
        for (name, body) in TEMPLATES {
            let doc = parse_document(body);
            assert!(
                !doc.blocks.is_empty(),
                "template {name} should parse into at least one block"
            );
        }
    }
}

/// 利用者が保存したテンプレート（`%APPDATA%\Typer\templates\*.md`）1件分
#[derive(Debug, Clone, PartialEq)]
pub struct UserTemplate {
    /// メニューに出す名前（ファイル名から `.md` を除いたもの）
    pub name: String,
    pub path: std::path::PathBuf,
}

/// 利用者のテンプレートを置くフォルダ
pub fn user_templates_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(
        std::path::PathBuf::from(appdata)
            .join("Typer")
            .join("templates"),
    )
}

/// フォルダにあるテンプレートを名前順に返す（フォルダがなければ空）
pub fn list_user_templates(dir: &std::path::Path) -> Vec<UserTemplate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut templates: Vec<UserTemplate> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")))
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().to_string();
            Some(UserTemplate { name, path })
        })
        .collect();
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    templates
}

/// テンプレートの名前を、ファイル名として使える形にする（空なら `None`）
fn template_file_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!(
        "{}.md",
        crate::ui_persistence::sanitize_file_name(trimmed)
    ))
}

/// テンプレートとして保存する。`overwrite` が偽で同じ名前がすでにあれば `Err`
pub fn save_user_template(
    dir: &std::path::Path,
    name: &str,
    content: &str,
    overwrite: bool,
) -> Result<std::path::PathBuf, String> {
    let file_name = template_file_name(name).ok_or("テンプレートの名前を入力してください")?;
    let path = dir.join(file_name);
    if path.exists() && !overwrite {
        return Err("同じ名前のテンプレートがすでにあります".to_string());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("フォルダを作れませんでした: {e}"))?;
    std::fs::write(&path, content).map_err(|e| format!("保存できませんでした: {e}"))?;
    Ok(path)
}

/// テンプレートの名前を変える。変更先の名前がすでにあれば `Err`
pub fn rename_user_template(
    template: &UserTemplate,
    new_name: &str,
) -> Result<std::path::PathBuf, String> {
    let file_name = template_file_name(new_name).ok_or("新しい名前を入力してください")?;
    let dir = template
        .path
        .parent()
        .ok_or("テンプレートの場所が分かりません")?;
    let new_path = dir.join(file_name);
    if new_path == template.path {
        return Ok(new_path);
    }
    if new_path.exists() {
        return Err("同じ名前のテンプレートがすでにあります".to_string());
    }
    std::fs::rename(&template.path, &new_path)
        .map_err(|e| format!("名前を変更できませんでした: {e}"))?;
    Ok(new_path)
}

#[cfg(test)]
mod user_template_tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("typer-templates-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn save_list_rename_and_refuse_overwrite() {
        let dir = temp_dir("crud");
        assert!(list_user_templates(&dir).is_empty());

        let path = save_user_template(&dir, "月次報告", "---\ntitle: 月次\n---\n", false).unwrap();
        assert_eq!(path.file_name().unwrap(), "月次報告.md");
        save_user_template(&dir, "議事録（営業部）", "本文", false).unwrap();
        // 同じ名前は上書きの確認なしでは保存しない
        assert!(save_user_template(&dir, "月次報告", "別", false).is_err());
        save_user_template(&dir, "月次報告", "上書き", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "上書き");
        // 名前が空なら保存しない／ファイル名に使えない文字は置き換える
        assert!(save_user_template(&dir, "  ", "x", false).is_err());
        let odd = save_user_template(&dir, "a/b:c", "x", false).unwrap();
        assert!(!odd.file_name().unwrap().to_string_lossy().contains('/'));

        // .md 以外のファイルは一覧に出さない
        std::fs::write(dir.join("memo.txt"), "x").unwrap();
        let names: Vec<String> = list_user_templates(&dir)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"月次報告".to_string()));

        let monthly = list_user_templates(&dir)
            .into_iter()
            .find(|t| t.name == "月次報告")
            .unwrap();
        // 既にある名前への変更は断る
        assert!(rename_user_template(&monthly, "議事録（営業部）").is_err());
        let renamed = rename_user_template(&monthly, "月次報告（2027年版）").unwrap();
        assert!(renamed.exists() && !monthly.path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
