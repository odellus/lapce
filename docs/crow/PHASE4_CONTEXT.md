# Phase 4: Typst Editor — Side-by-Side Preview + PDF Export

## Goal

When a user opens a `.typ` file, they can open a live Typst preview tab alongside the editor. The preview recompiles on every buffer change (debounced ~150ms) and renders via the floem-typst crate. Also add a PDF export command.

## Architecture

Follow the exact same pattern as `EditorTabChild::Settings` / `EditorTabChild::Keymap` — these are the simplest non-editor tab types.

### Files to create

**`lapce-app/src/typst_preview.rs`** (~200 LOC)

```rust
pub struct TypstPreviewData {
    pub id: TypstPreviewId,
    pub path: PathBuf,
    // The compiled scene, updated on buffer change
    pub scene: RwSignal<Option<Scene>>,  // from floem-typst
    pub error: RwSignal<Option<String>>,
    pub is_compiling: RwSignal<bool>,
}
```

- On creation, find the Doc for `path` in `main_split.docs`
- Use `create_effect` watching `doc.buffer` (via `doc.rev()` or `doc.cache_rev`) to detect changes
- Debounce: use `std::thread::sleep` in a spawned thread, or a simple counter approach
- Compile: use `typst::compile()` with a `StreamWorld` (from floem-typst crate) or a simple `TypstWorld` impl
- On success: extract `PagedDocument` → convert to floem-typst `Scene` → update signal
- On error: set error signal with diagnostic messages

**View function** in the same file:

```rust
pub fn typst_preview_view(data: &TypstPreviewData, config: ReadSignal<Arc<LapceConfig>>) -> impl View {
    // If error: show error text in red
    // If scene: render using floem-typst's paint_frame or TypstView
    // If compiling: show spinner
}
```

### Files to modify

**`lapce-app/src/id.rs`**
```rust
pub type TypstPreviewId = Id;  // add after VoltViewId
```

**`lapce-app/src/editor_tab.rs`**
- Add `TypstPreview { path: PathBuf }` to `EditorTabChildSource`
- Add `TypstPreview(TypstPreviewId)` to `EditorTabChild`
- Add `TypstPreview { path: PathBuf }` to `EditorTabChildInfo`
- Handle in `id()`, `child_info()`, `view_info()`, `to_data()`
- For `view_info()`: icon = file icon for the .typ path, name = filename + " (Preview)"

**`lapce-app/src/main_split.rs`**
- Add `typst_previews: RwSignal<im::HashMap<TypstPreviewId, TypstPreviewData>>` to `MainSplitData`
- Add `open_typst_preview(&self, path: PathBuf)` method (like `open_settings`)
- Handle `EditorTabChildSource::TypstPreview` in `get_editor_tab_child` match
- Handle `EditorTabChild::TypstPreview` in all the match arms (selected, is_same, save position, etc.)
- For `is_same`: match on path equality

**`lapce-app/src/app.rs`** (~line 1400)
- Add view dispatch:
```rust
EditorTabChild::TypstPreview(id) => {
    let preview_data = main_split.typst_previews.get_untracked().get(&id).cloned();
    if let Some(data) = preview_data {
        typst_preview_view(&data, common.config).into_any()
    } else {
        text("empty typst preview").into_any()
    }
}
```

**`lapce-app/src/command.rs`**
- Add `OpenTypstPreview` to `InternalCommand` enum

**`lapce-app/src/window_tab.rs`**
- Handle `OpenTypstPreview`: get the active editor's doc path, if it ends in `.typ`, call `main_split.open_typst_preview(path)`

### Compilation (typst)

Use the typst crates directly (they're already in the workspace via floem-typst's path deps):

```rust
use typst::compile;
use typst::World;
```

You need a simple `World` impl that:
- Has a `main` file (the .typ content from the doc buffer)
- Loads system fonts via `fontdb` (copy the pattern from `floem-typst/src/world.rs`)
- Returns `FileId::new(None, "main.typ")` as the main file

The compile call:
```rust
let world = PreviewWorld::new(source_text);
let result = typst::compile(&world);
match result.output {
    Ok(doc) => { /* doc is PagedDocument */ }
    Err(errors) => { /* format diagnostics */ }
}
```

### Rendering the PagedDocument

The `PagedDocument` has `pages: Vec<Page>` where each `Page` has a `frame: Frame`.

Use the `paint_frame` function from `floem-typst/src/render.rs` to draw each page's frame. Or if that's too complex for now, just render page 1's frame.

**Important:** floem-typst is at `~/src/crow-team/floem-typst/`. Add it as a dependency to lapce-app's Cargo.toml:
```toml
floem-typst = { path = "../../floem-typst" }
```

Also add typst deps to lapce-app/Cargo.toml:
```toml
typst = { path = "../../typst/crates/typst" }
typst-layout = { path = "../../typst/crates/typst-layout" }
typst-pdf = { path = "../../typst/crates/typst-pdf" }
fontdb = "0.23"
```

### PDF Export

Add `ExportTypstPdf` to `InternalCommand`. Handler in window_tab.rs:
1. Get active editor's doc path (must be .typ)
2. Get buffer text
3. Compile with typst
4. Call `typst_pdf::pdf(&doc, &PdfOptions::default())`
5. Write to same path but with `.pdf` extension
6. Show status message

### Debounce Strategy

Simple approach: use a `RwSignal<u64>` counter. On buffer change, increment counter. Spawn a thread that:
1. Reads counter
2. Sleeps 150ms
3. Reads counter again — if unchanged, compile
4. If changed, loop back to step 1

Or use `create_effect` with a `watch` on the buffer rev and a `setTimeout`-like mechanism. The thread approach is simpler in Rust.

### What NOT to do

- Don't add tree-sitter syntax highlighting for .typ yet (that's complex, skip for now)
- Don't try to make the preview update character-by-character (debounced full recompile is fine)
- Don't modify the floem-typst crate — use it as-is
- Don't add side-by-side split automatically — just open a new tab (user can drag to split)

### Build verification

```bash
cd ~/src/crow-team/lapce && cargo check 2>&1 | tail -5
```

Must compile with 0 errors. Warnings are OK.
