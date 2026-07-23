//! Typst World implementation for the streaming compiler.

use std::path::PathBuf;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::LibraryExt;
use typst_library::World;

/// A minimal Typst world for streaming compilation.
pub struct StreamWorld {
    source: Source,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    library: LazyHash<typst_library::Library>,
}

impl StreamWorld {
    pub fn new() -> Self {
        let (book, fonts) = load_system_fonts();

        let source = Source::new(
            FileId::new(RootedPath::new(
                VirtualRoot::Project,
                VirtualPath::new("main.typ").unwrap(),
            )),
            String::new(),
        );

        Self {
            source,
            book: LazyHash::new(book),
            fonts,
            library: LazyHash::new(typst_library::Library::default()),
        }
    }

    pub fn set_source(&mut self, text: &str) {
        self.source = Source::new(
            FileId::new(RootedPath::new(
                VirtualRoot::Project,
                VirtualPath::new("main.typ").unwrap(),
            )),
            text.to_string(),
        );
    }
}

impl World for StreamWorld {
    fn library(&self) -> &LazyHash<typst_library::Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::NotFound(PathBuf::from(
                id.vpath().get_without_slash().to_string(),
            )))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(PathBuf::from(
            id.vpath().get_without_slash().to_string(),
        )))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        let _ = offset;
        Datetime::from_ymd(2026, 7, 19)
    }
}

fn load_system_fonts() -> (FontBook, Vec<Font>) {
    let mut font_db = fontdb::Database::new();
    font_db.load_system_fonts();

    let mut book = FontBook::new();
    let mut fonts = Vec::new();

    for face in font_db.faces() {
        let data: Option<Vec<u8>> = match &face.source {
            fontdb::Source::File(path) => std::fs::read(path).ok(),
            fontdb::Source::Binary(data) => Some(data.as_ref().as_ref().to_vec()),
            _ => None,
        };

        if let Some(data) = data {
            for font in Font::iter(Bytes::new(data.clone())) {
                book.push(font.info().clone());
                fonts.push(font);
            }
        }
    }

    (book, fonts)
}
