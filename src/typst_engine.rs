//! Typst インメモリ組版エンジン・プレビュー描画・PDF出力モジュール
//!
//! 外部CLIプロセスを起動せず、Rustプロセス内で直接Typstエンジンを実行する。
//! Windowsの日本語システムフォントをプロセス起動時に一度だけ読み込んで共有キャッシュ（`OnceLock`）し、
//! `Source::replace` による差分更新と `comemo` のキャッシュ機構を活用して高速なプレビューとPDF出力を実現する。

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source};
use typst::WorldExt;
use typst_syntax::DiagSpan;
use typst::text::{Font, FontBook};
use typst::utils::{LazyHash, Scalar};
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions;
use typst_render::RenderOptions;

use crate::typst_gen::decode_local_image_vpath;

struct SharedTypstResources {
    library: Arc<LazyHash<Library>>,
    book: Arc<LazyHash<FontBook>>,
    fonts: Arc<Vec<Font>>,
}

static SHARED_RESOURCES: OnceLock<SharedTypstResources> = OnceLock::new();

fn get_shared_resources() -> &'static SharedTypstResources {
    SHARED_RESOURCES.get_or_init(|| {
        let fonts = load_windows_japanese_fonts();
        let book = FontBook::from_fonts(&fonts);
        SharedTypstResources {
            library: Arc::new(LazyHash::new(Library::default())),
            book: Arc::new(LazyHash::new(book)),
            fonts: Arc::new(fonts),
        }
    })
}

pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8 ピクセル列（`egui::ColorImage::from_rgba_premultiplied` に直接渡せる形式）
    pub premultiplied_rgba: Vec<u8>,
}

pub struct PreviewOutput {
    pub pages: Vec<RenderedPage>,
    pub compile_ms: f64,
}

/// Typst の `World` トレイト実装
pub struct TypstEngine {
    library: Arc<LazyHash<Library>>,
    book: Arc<LazyHash<FontBook>>,
    fonts: Arc<Vec<Font>>,
    main_source: Source,
}

impl TypstEngine {
    /// 共有フォントキャッシュを利用してエンジンを即座に初期化する
    pub fn new() -> Self {
        let shared = get_shared_resources();
        Self {
            library: Arc::clone(&shared.library),
            book: Arc::clone(&shared.book),
            fonts: Arc::clone(&shared.fonts),
            main_source: Source::detached(String::new()),
        }
    }

    /// Typstソース文字列を差分更新してコンパイルし、`PagedDocument` を返す
    fn compile_document(&mut self, typst_code: &str) -> Result<PagedDocument, String> {
        self.main_source.replace(typst_code);
        let warned = typst::compile::<PagedDocument>(self);
        match warned.output {
            Ok(doc) => Ok(doc),
            Err(errors) => {
                let mut msg = String::new();
                for err in errors.iter() {
                    if !msg.is_empty() {
                        msg.push('\n');
                    }
                    if let Some(line) = self.span_to_line(err.span) {
                        msg.push_str(&format!("[生成コード {line}行目] "));
                    }
                    msg.push_str(&err.message);
                }
                Err(msg)
            }
        }
    }

    /// エラーのspanから、生成されたTypstソース上の1始まりの行番号を求める
    fn span_to_line(&self, span: DiagSpan) -> Option<usize> {
        let byte_offset = self.range(span)?.start;
        let (line, _col) = self.main_source.lines().byte_to_line_column(byte_offset)?;
        Some(line + 1)
    }

    /// プレビュー用の各ページ画像をメモリ上でレンダリングする
    pub fn render_preview(
        &mut self,
        typst_code: &str,
        pixel_per_pt: f32,
    ) -> Result<PreviewOutput, String> {
        let start = Instant::now();
        let doc = self.compile_document(typst_code)?;

        let opts = RenderOptions {
            pixel_per_pt: Scalar::new(pixel_per_pt.clamp(0.5, 4.0) as f64),
            render_bleed: false,
        };

        let mut pages = Vec::with_capacity(doc.pages().len());
        for page in doc.pages() {
            let pixmap = typst_render::render(page, &opts);
            pages.push(RenderedPage {
                width: pixmap.width(),
                height: pixmap.height(),
                premultiplied_rgba: pixmap.take(),
            });
        }

        let compile_ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok(PreviewOutput { pages, compile_ms })
    }

    /// PDFバイナリをメモリ上で生成して返す
    pub fn export_pdf(&mut self, typst_code: &str) -> Result<Vec<u8>, String> {
        let doc = self.compile_document(typst_code)?;
        let options = PdfOptions::default();
        typst_pdf::pdf(&doc, &options).map_err(|errs| {
            errs.iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
    }
}

impl World for TypstEngine {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main_source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main_source.id() {
            Ok(self.main_source.clone())
        } else {
            Err(FileError::NotFound(
                Path::new(id.vpath().get_without_slash()).into(),
            ))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main_source.id() {
            return Ok(Bytes::from_string(self.main_source.text().to_owned()));
        }

        let vpath_str = id.vpath().get_without_slash();
        if let Some(real_path) = decode_local_image_vpath(vpath_str) {
            let bytes = std::fs::read(&real_path)
                .map_err(|e| FileError::from_io(e, &real_path))?;
            return Ok(Bytes::new(bytes));
        }

        Err(FileError::NotFound(Path::new(vpath_str).into()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        Datetime::from_ymd(2026, 10, 16)
    }
}

/// Windows標準の日本語フォント・欧文フォントを優先順で読み込む
fn load_windows_japanese_fonts() -> Vec<Font> {
    let mut fonts = Vec::new();
    let font_dir = Path::new(r"C:\Windows\Fonts");

    let candidate_files = [
        // BIZ UDゴシック / 明朝（業務報告書で非常に視認性が高い）
        "BIZ-UDGothicR.ttc",
        "BIZ-UDGothicB.ttc",
        "BIZ-UDMinchoM.ttc",
        // 游ゴシック / 游明朝
        "YuGothM.ttc",
        "YuGothB.ttc",
        "yumin.ttf",
        "yumindb.ttf",
        // コード用等幅・欧文フォント
        "consola.ttf",
        "consolab.ttf",
        "arial.ttf",
        "arialbd.ttf",
        // 絵文字（COLR形式。typst-renderが対応していない場合は無効)
        "seguiemj.ttf",
    ];

    for filename in candidate_files {
        let path = font_dir.join(filename);
        if let Ok(bytes) = std::fs::read(&path) {
            let shared_bytes = Bytes::new(bytes);
            for font in Font::iter(shared_bytes) {
                fonts.push(font);
            }
        }
    }

    // Typst公式が組版・数式表示用に同梱しているフォント（New Computer Modern Math等）を追加する。
    // これがないと `$alpha + beta$` のような数式記法・ギリシャ文字・特殊記号が
    // 対応フォント無しでグリフ欠落（四角表示）になる
    for static_bytes in typst_assets::fonts() {
        let shared_bytes = Bytes::new(static_bytes);
        for font in Font::iter(shared_bytes) {
            fonts.push(font);
        }
    }

    fonts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html_export::generate_html;
    use crate::parser::parse_document;
    use crate::sample::SAMPLE_DOCUMENT;
    use crate::typst_gen::generate_typst_source;

    #[test]
    fn test_compile_error_includes_generated_code_line_number() {
        // ```typst フェンスで意図的に壊れたTypstコード（未閉包の角括弧）を渡し、
        // コンパイルエラーのメッセージに生成コード上の行番号が含まれることを確認する
        let src = "本文\n\n```typst\n#box[閉じていない\n```\n";
        let doc = parse_document(src);
        let typst_code = generate_typst_source(&doc);
        let mut engine = TypstEngine::new();

        let err = match engine.render_preview(&typst_code, 1.5) {
            Err(e) => e,
            Ok(_) => panic!("unbalanced bracket should fail to compile"),
        };
        assert!(
            err.contains("[生成コード") && err.contains("行目]"),
            "error message should include a generated-code line number: {err}"
        );
    }

    #[test]
    fn test_end_to_end_sample_document_render_and_pdf() {
        let doc = parse_document(SAMPLE_DOCUMENT);
        let typst_code = generate_typst_source(&doc);
        let mut engine = TypstEngine::new();

        let preview = engine
            .render_preview(&typst_code, 1.5)
            .expect("Preview rendering should succeed");
        assert!(!preview.pages.is_empty(), "Should render at least 1 page");
        assert!(preview.pages[0].width > 100);
        assert!(preview.pages[0].height > 100);

        let pdf_bytes = engine
            .export_pdf(&typst_code)
            .expect("PDF export should succeed");
        assert!(
            pdf_bytes.starts_with(b"%PDF-"),
            "Generated bytes should be a valid PDF"
        );

        let html = generate_html(&doc);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<colgroup>"));
    }

    #[test]
    fn test_math_notation_renders_without_error() {
        // typst-assetsのNewCMMathフォント追加後、数式記法（ギリシャ文字・分数・総和等）を
        // 含む文書がエラーなく組版できることを確認する（実際のグリフ表示は目視確認済み）
        let src = "```typst\n$alpha + beta = gamma$\n$ sum_(n=1)^infinity 1 / n^2 = pi^2 / 6 $\n```\n";
        let doc = parse_document(src);
        let typst_code = generate_typst_source(&doc);
        let mut engine = TypstEngine::new();

        let preview = engine
            .render_preview(&typst_code, 1.5)
            .expect("Preview rendering with math notation should not fail");
        assert!(!preview.pages.is_empty());
    }

    #[test]
    fn test_emoji_in_document_renders_without_error() {
        // Segoe UI Emoji フォント追加後も、絵文字を含む文書が組版エラーなく
        // プレビュー・PDF出力できることを確認する（実際のグリフ表示の目視確認は別途実施済み）
        let src = "# 絵文字テスト\n\n📖 🕒 📂 通常の文字\n";
        let doc = parse_document(src);
        let typst_code = generate_typst_source(&doc);
        let mut engine = TypstEngine::new();

        let preview = engine
            .render_preview(&typst_code, 1.5)
            .expect("Preview rendering with emoji should not fail");
        assert!(!preview.pages.is_empty());

        let pdf_bytes = engine
            .export_pdf(&typst_code)
            .expect("PDF export with emoji should not fail");
        assert!(pdf_bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn test_block_width_attribute_compiles_and_renders() {
        // {block-width: ...} 指定を含む文書が実際にTypstコンパイル・プレビュー描画・
        // PDF出力までエラーなく通ることを確認する（生成コード検証は typst_gen 側で実施済み）
        let src = "{align: center, block-width: 60%}\n幅60%で中央寄せの段落。\n\n{align: right, block-width: 40%}\n幅40%で右寄せの段落。\n";
        let doc = parse_document(src);
        let typst_code = generate_typst_source(&doc);
        assert!(typst_code.contains("#block(width: 60%)"));
        assert!(typst_code.contains("#block(width: 40%)"));

        let mut engine = TypstEngine::new();
        let preview = engine
            .render_preview(&typst_code, 1.5)
            .expect("block-width document should render without error");
        assert!(!preview.pages.is_empty());

        let pdf_bytes = engine
            .export_pdf(&typst_code)
            .expect("block-width document should export to PDF without error");
        assert!(pdf_bytes.starts_with(b"%PDF-"));
    }

    #[test]
    #[ignore]
    fn bench_compile_render_pdf_at_scale() {
        for repeat in [1usize, 10, 50, 200] {
            let text = SAMPLE_DOCUMENT.repeat(repeat);
            let doc = parse_document(&text);
            let typst_code = generate_typst_source(&doc);
            let mut engine = TypstEngine::new();

            let start = Instant::now();
            let preview = engine
                .render_preview(&typst_code, 2.0)
                .expect("preview rendering should succeed");
            let preview_elapsed = start.elapsed();

            let start = Instant::now();
            let pdf_bytes = engine.export_pdf(&typst_code).expect("pdf export should succeed");
            let pdf_elapsed = start.elapsed();

            println!(
                "repeat={repeat:>4} chars={:>9} | pages={:>3} preview={preview_elapsed:>10?} | pdf={pdf_elapsed:>10?} ({} bytes)",
                text.chars().count(),
                preview.pages.len(),
                pdf_bytes.len()
            );
        }
    }
}
