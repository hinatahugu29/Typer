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
