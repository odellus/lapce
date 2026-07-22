use std::path::PathBuf;
use std::rc::Rc;

use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};

use crate::window_tab::CommonData;

#[derive(Clone, Debug)]
pub struct NoteFile {
    pub path: PathBuf,
    pub name: String,
    pub is_typst: bool,
}

#[derive(Clone)]
pub struct NotesData {
    pub files: RwSignal<Vec<NoteFile>>,
    pub selected: RwSignal<Option<usize>>,
    pub content: RwSignal<String>,
    pub common: Rc<CommonData>,
}

impl NotesData {
    pub fn new(cx: Scope, common: Rc<CommonData>) -> Self {
        let files = cx.create_rw_signal(Vec::new());
        let selected = cx.create_rw_signal(None);
        let content = cx.create_rw_signal(String::new());

        let data = Self {
            files,
            selected,
            content,
            common,
        };

        data.scan_workspace();
        data
    }

    /// Scan the workspace for .md and .typ files.
    pub fn scan_workspace(&self) {
        let workspace_path = self.common.workspace.path.clone();
        let files = self.files;

        let Some(root) = workspace_path else {
            files.set(Vec::new());
            return;
        };

        let mut found = Vec::new();
        Self::scan_dir(&root, &root, &mut found, 0);
        found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.set(found);
    }

    fn scan_dir(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<NoteFile>,
        depth: usize,
    ) {
        if depth > 5 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.starts_with('.')
                    || name == "node_modules"
                    || name == "target"
                    || name == "dist"
                    || name == "build"
                    || name == "__pycache__"
                {
                    continue;
                }
                Self::scan_dir(root, &path, out, depth + 1);
            } else if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                if ext == "md" || ext == "typ" {
                    let name = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    out.push(NoteFile {
                        is_typst: ext == "typ",
                        path,
                        name,
                    });
                }
            }
        }
    }

    /// Select a file by index and load its content.
    pub fn select_file(&self, index: usize) {
        let file = self.files.with(|f| f.get(index).cloned());
        let Some(file) = file else { return };

        let content = std::fs::read_to_string(&file.path).unwrap_or_default();
        self.content.set(content);
        self.selected.set(Some(index));
    }

    /// Get the currently selected file.
    pub fn selected_file(&self) -> Option<NoteFile> {
        let idx = self.selected.get()?;
        self.files.with(|f| f.get(idx).cloned())
    }

    /// Push the selected file's content to the TypstView (for .typ files).
    pub fn render_in_typst_view(&self, typst_view: &floem_typst::TypstView) {
        let file = self.selected_file();
        let Some(file) = file else { return };
        if !file.is_typst {
            return;
        }
        let content = self.content.get();
        typst_view.reset();
        typst_view.push(&content);
        typst_view.flush();
    }
}
