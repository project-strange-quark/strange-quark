use rusqlite::Connection;
use serde::Serialize;
use std::sync::Mutex;
use walkdir::WalkDir;

#[derive(Serialize)]
struct Photo { path: String, width: u32, height: u32, tags: Vec<String> }

struct Db(Mutex<Connection>);

fn tags_for(db: &Connection, path: &str) -> Vec<String> {
    let mut q = db.prepare("SELECT tag FROM tags WHERE path = ?1 ORDER BY tag").unwrap();
    q.query_map([path], |r| r.get(0)).unwrap().filter_map(Result::ok).collect()
}

#[tauri::command]
fn scan_folder(folder: String, db: tauri::State<Db>) -> Vec<Photo> {
    let db = db.0.lock().unwrap();
    WalkDir::new(&folder).into_iter().filter_map(Result::ok)
        .filter(|e| matches!(e.path().extension().and_then(|x| x.to_str()), Some("jpg" | "jpeg" | "png" | "webp")))
        .filter_map(|e| {
            let path = e.path().to_string_lossy().into_owned();
            let (width, height) = image::image_dimensions(e.path()).ok()?;
            let tags = tags_for(&db, &path);
            Some(Photo { path, width, height, tags })
        })
        .collect()
}

#[tauri::command]
fn add_tag(path: String, tag: String, db: tauri::State<Db>) -> Vec<String> {
    let db = db.0.lock().unwrap();
    db.execute("INSERT OR IGNORE INTO tags (path, tag) VALUES (?1, ?2)", [&path, &tag]).unwrap();
    tags_for(&db, &path)
}

fn main() {
    let db = Connection::open("phototag.db").unwrap();
    db.execute("CREATE TABLE IF NOT EXISTS tags (path TEXT, tag TEXT, UNIQUE(path, tag))", []).unwrap();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(Db(Mutex::new(db)))
        .invoke_handler(tauri::generate_handler![scan_folder, add_tag])
        .run(tauri::generate_context!())
        .expect("error while running PhotoTag");
}
