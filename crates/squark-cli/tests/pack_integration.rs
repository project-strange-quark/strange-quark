// SPDX-License-Identifier: MIT
//! Integration tests for `squark pack`, `squark inspect --dump-pack`, `document.bin`
//! compilation, golden fixtures shared with `squark-assets`, and malformed-input rejection.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use squark_cli::html::compile_html_to_document_bin;
use squark_cli::inspect::{ContainerKind, inspect_bytes, inspect_path, validate_document_bin};
use squark_cli::pack::{MimeSpec, PackBuilder, PackOptions, append_pack_to_binary, pack_directory};
use squark_journal::BatchDecoder;
use squark_types::NodeId;
use squark_types::journal::{AtomKind, MAX_RECORD_BYTES, NodeType, Opcode, RECORD_SIZE};
use squark_types::pack::{
    BATCH_DIR_ENTRY_SIZE, BatchDirEntry, DOCUMENT_HEADER_SIZE, DOCUMENT_PATH, DocumentHeader,
    MANIFEST_PATH, MIME_CUSTOM, PACK_HEADER_SIZE, QuirksMode, ScriptFlags, ScriptKind,
    TRAILER_SIZE,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pack")
}

fn squark_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_squark"))
}

fn fake_runtime_prefix() -> Vec<u8> {
    b"SQUARK-FAKE-RUNTIME".repeat(8) // 128 bytes, non-4096-aligned to test page padding
}

fn check_or_update_golden(path: &Path, actual: &[u8]) {
    if std::env::var_os("SQUARK_UPDATE_GOLDEN").is_some() {
        fs::write(path, actual).expect("write golden fixture");
    } else {
        let expected = fs::read(path).unwrap_or_else(|err| {
            panic!(
                "missing golden fixture {} ({err}); run with SQUARK_UPDATE_GOLDEN=1 to generate",
                path.display()
            )
        });
        assert_eq!(
            actual,
            expected.as_slice(),
            "golden fixture {} drifted from writer output",
            path.display()
        );
    }
}

#[test]
fn golden_basic_pack_and_appended_binary_match_byte_for_byte() {
    let dir = fixtures_dir();
    let basic_site = dir.join("basic-site");
    let tmp = std::env::temp_dir().join(format!("squark-cli-golden-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let runtime_path = tmp.join("fake-runtime");
    fs::write(&runtime_path, fake_runtime_prefix()).unwrap();

    let out_sqpak = tmp.join("basic.sqpak");
    let out_appended = tmp.join("basic.appended.bin");

    let summary = pack_directory(&PackOptions {
        web_dir: basic_site,
        html_path: None,
        out_sqpak: Some(out_sqpak.clone()),
        out_binary: Some(out_appended.clone()),
        runtime_binary: Some(runtime_path),
        include_sourcemaps: false,
        app_name: Some("basic-site".to_owned()),
        skip_other_html: false,
        window: None,
        network_allow: Vec::new(),
        extra_assets: Vec::new(),
        native_modules: Vec::new(),
        prelude: None,
    })
    .unwrap();

    let sqpak_bytes = fs::read(&out_sqpak).unwrap();
    let appended_bytes = fs::read(&out_appended).unwrap();

    check_or_update_golden(&dir.join("basic.sqpak"), &sqpak_bytes);
    check_or_update_golden(&dir.join("basic.document.bin"), &summary.document.bytes);
    check_or_update_golden(&dir.join("basic.appended.bin"), &appended_bytes);

    let inspection = inspect_bytes(&appended_bytes).unwrap();
    let inspect_json = format!("{}\n", serde_json::to_string_pretty(&inspection).unwrap());
    check_or_update_golden(&dir.join("basic.inspect.json"), inspect_json.as_bytes());

    // Validate inspection details on the golden appended binary.
    assert_eq!(inspection.container, ContainerKind::AppendedBinary);
    let trailer = inspection.trailer.as_ref().unwrap();
    assert_eq!(trailer.pack_offset, 4096);
    assert_eq!(trailer.pack_len as usize, sqpak_bytes.len());
    assert!(trailer.crc_valid);

    let pack = inspection.pack.as_ref().unwrap();
    let paths: Vec<&str> = pack.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            DOCUMENT_PATH,
            MANIFEST_PATH,
            "/assets/main.js",
            "/data/config.json",
            "/fonts/sample.woff2",
            "/images/dot.png",
            "/index.html",
            "/styles/app.css",
        ]
    );
    // Sourcemap `/assets/main.js.map` must be excluded by default.
    assert!(!paths.contains(&"/assets/main.js.map"));

    // Validate compiled document.bin semantics through squark-journal's BatchDecoder.
    let doc = validate_document_bin(&summary.document.bytes).unwrap();
    assert_eq!(doc.header.quirks_mode, QuirksMode::Standards);
    assert_eq!(doc.header.document_node_id, NodeId::new(1, 0).unwrap());
    assert_eq!(doc.batches.len(), 1);

    // Check scripts & stylesheets in document order (JSON script excluded).
    assert_eq!(doc.scripts.len(), 5);
    let script_info: Vec<_> = doc
        .scripts
        .iter()
        .map(|s| {
            (
                s.kind,
                s.flags,
                String::from_utf16_lossy(&doc.script_strings[s.payload_str as usize]),
                String::from_utf16_lossy(&doc.script_strings[s.base_url_str as usize]),
            )
        })
        .collect();
    assert_eq!(
        script_info,
        vec![
            (
                ScriptKind::ExternalStylesheet,
                ScriptFlags::NONE,
                "/styles/app.css".to_owned(),
                "/styles/app.css".to_owned(),
            ),
            (
                ScriptKind::InlineClassic,
                ScriptFlags::NONE,
                "window.__boot = 1;".to_owned(),
                "/index.html".to_owned(),
            ),
            (
                ScriptKind::ExternalModule,
                ScriptFlags::NONE,
                "/assets/main.js".to_owned(),
                "/assets/main.js".to_owned(),
            ),
            (
                ScriptKind::ExternalClassic,
                ScriptFlags::DEFER.union(ScriptFlags::ASYNC),
                "https://cdn.example.test/ext.js".to_owned(),
                "https://cdn.example.test/ext.js".to_owned(),
            ),
            (
                ScriptKind::InlineModule,
                ScriptFlags::NONE,
                "import \"./assets/main.js\";".to_owned(),
                "/index.html".to_owned(),
            ),
        ]
    );

    // Check namespaces, qualified xlink:href attr, doctype, comment, SHEET_INSERT, and non-BMP UTF-16 text.
    let mut saw_xhtml = false;
    let mut saw_svg = false;
    let mut saw_xlink_href = false;
    let mut style_tag_atom = None;
    for (&atom_id, &(kind, ref units)) in &doc.atoms {
        let s = String::from_utf16_lossy(units);
        if kind == AtomKind::Namespace && s == "http://www.w3.org/1999/xhtml" {
            saw_xhtml = true;
        }
        if kind == AtomKind::Namespace && s == "http://www.w3.org/2000/svg" {
            saw_svg = true;
        }
        if kind == AtomKind::QualifiedAttribute && s == "http://www.w3.org/1999/xlink\0xlink:href" {
            saw_xlink_href = true;
        }
        if kind == AtomKind::Tag && s == "style" {
            style_tag_atom = Some(atom_id);
        }
    }
    let mut style_elem_id = None;
    let mut saw_sheet_insert = false;
    let mut saw_doctype = false;
    let mut saw_comment = false;
    let mut saw_emoji = false;
    for rec in &doc.batches[0].records {
        if rec.op == Opcode::CreateElement && Some(rec.b) == style_tag_atom {
            style_elem_id = Some(rec.a);
        }
        if rec.op == Opcode::SheetInsert {
            assert_eq!(
                Some(rec.a),
                style_elem_id,
                "SHEET_INSERT id must match <style> NodeId"
            );
            assert_eq!(rec.b, 0);
            assert_eq!(
                doc.batches[0].string_lossy(rec.c).as_deref(),
                Some("body { margin: 0; color: #222; }")
            );
            saw_sheet_insert = true;
        }
        if rec.op == Opcode::CreateNode && rec.b == NodeType::DocumentType as u32 {
            assert_eq!(doc.batches[0].string_lossy(rec.c).as_deref(), Some("html"));
            saw_doctype = true;
        }
        if rec.op == Opcode::CreateNode && rec.b == NodeType::Comment as u32 {
            assert_eq!(
                doc.batches[0].string_lossy(rec.c).as_deref(),
                Some(" golden comment ")
            );
            saw_comment = true;
        }
        if rec.op == Opcode::CreateText
            && doc.batches[0].string_lossy(rec.b).as_deref() == Some("Hello 🌍")
        {
            // Non-BMP emoji must encode as a UTF-16 surrogate pair (2 code units).
            assert_eq!(doc.batches[0].strings[rec.b as usize].len(), 8);
            saw_emoji = true;
        }
    }
    assert!(saw_xhtml, "expected XHTML namespace atom");
    assert!(saw_svg, "expected SVG namespace atom");
    assert!(saw_xlink_href, "expected qualified xlink:href atom");
    assert!(saw_sheet_insert, "expected SHEET_INSERT for inline <style>");
    assert!(saw_doctype, "expected CREATE_NODE doctype");
    assert!(saw_comment, "expected CREATE_NODE comment");
    assert!(saw_emoji, "expected UTF-16 surrogate pair text");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn golden_quirks_modes_and_custom_mime_match_byte_for_byte() {
    let dir = fixtures_dir();

    let quirks =
        compile_html_to_document_bin("<html><body>quirks</body></html>", "/index.html").unwrap();
    assert_eq!(quirks.quirks_mode, QuirksMode::Quirks);
    check_or_update_golden(&dir.join("quirks.document.bin"), &quirks.bytes);

    let limited = compile_html_to_document_bin(
        "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \"http://www.w3.org/TR/html4/loose.dtd\"><html><body>limited</body></html>",
        "/index.html",
    )
    .unwrap();
    assert_eq!(limited.quirks_mode, QuirksMode::LimitedQuirks);
    check_or_update_golden(&dir.join("limited-quirks.document.bin"), &limited.bytes);

    let mut builder = PackBuilder::new();
    builder
        .add_entry(
            "/data/sample.custom",
            MimeSpec::from_mime("application/vnd.squark.test"),
            b"custom-payload".to_vec(),
        )
        .unwrap();
    let custom_sqpak = builder.build().unwrap();
    check_or_update_golden(&dir.join("custom-mime.sqpak"), &custom_sqpak);

    let info = inspect_bytes(&custom_sqpak).unwrap();
    let pack = info.pack.unwrap();
    assert_eq!(pack.entries.len(), 1);
    assert_eq!(pack.entries[0].mime_id, MIME_CUSTOM);
    assert_eq!(pack.entries[0].mime, "application/vnd.squark.test");
}

#[test]
fn golden_template_content_fragment_links_owner_template_node_id() {
    let dir = fixtures_dir();
    let html = "<!DOCTYPE html><html><body><template id=\"tpl\"><div class=\"inner\">hello</div></template></body></html>";
    let compiled = compile_html_to_document_bin(html, "/index.html").unwrap();
    check_or_update_golden(&dir.join("template.document.bin"), &compiled.bytes);

    // Decode directly through squark-journal::BatchDecoder to prove
    // template content fragments validate with zero fallback.
    let header =
        DocumentHeader::decode(compiled.bytes[..DOCUMENT_HEADER_SIZE].try_into().unwrap()).unwrap();
    assert_eq!(header.batch_count, 1);
    let dir_off = header.batch_dir_offset as usize;
    let batch_dir = BatchDirEntry::decode(
        compiled.bytes[dir_off..dir_off + BATCH_DIR_ENTRY_SIZE]
            .try_into()
            .unwrap(),
    );
    let batch_slice =
        &compiled.bytes[batch_dir.offset as usize..(batch_dir.offset + batch_dir.len) as usize];
    let mut decoder = BatchDecoder::new(header.document_node_id).unwrap();
    let decoded = decoder.validate(batch_slice).unwrap();
    assert!(decoded.records().len() > 0);
    decoded.commit();

    let doc = validate_document_bin(&compiled.bytes).unwrap();
    let template_tag = doc
        .atoms
        .iter()
        .find_map(|(&id, (k, u))| {
            (*k == AtomKind::Tag && String::from_utf16_lossy(u) == "template").then_some(id)
        })
        .expect("template tag atom");
    let div_tag = doc
        .atoms
        .iter()
        .find_map(|(&id, (k, u))| {
            (*k == AtomKind::Tag && String::from_utf16_lossy(u) == "div").then_some(id)
        })
        .expect("div tag atom");

    let mut template_id = None;
    let mut template_pos = None;
    let mut frag_id = None;
    let mut frag_pos = None;
    let mut div_id = None;
    let mut inserted_into_frag = false;

    for (idx, rec) in doc.batches[0].records.iter().enumerate() {
        if rec.op == Opcode::CreateElement && rec.b == template_tag {
            template_id = Some(rec.a);
            template_pos = Some(idx);
        }
        if rec.op == Opcode::CreateNode && rec.b == NodeType::DocumentFragment as u32 {
            assert_eq!(
                Some(rec.c),
                template_id,
                "CREATE_NODE for template content fragment must set operand c to owning <template> NodeId"
            );
            frag_id = Some(rec.a);
            frag_pos = Some(idx);
        }
        if rec.op == Opcode::CreateElement && rec.b == div_tag {
            div_id = Some(rec.a);
        }
        if rec.op == Opcode::Insert && Some(rec.a) == frag_id && Some(rec.b) == div_id {
            inserted_into_frag = true;
        }
    }

    assert!(template_pos.unwrap() < frag_pos.unwrap());
    assert!(
        inserted_into_frag,
        "template child must insert into content fragment"
    );
}

#[test]
fn cli_pack_and_inspect_commands_work_end_to_end() {
    let dir = fixtures_dir();
    let tmp = std::env::temp_dir().join(format!("squark-cli-bin-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let runtime_path = tmp.join("runtime-stub");
    fs::write(&runtime_path, fake_runtime_prefix()).unwrap();
    let out_sqpak = tmp.join("out.sqpak");
    let out_bin = tmp.join("out-app");

    let status = Command::new(squark_bin())
        .args([
            "pack",
            dir.join("basic-site").to_str().unwrap(),
            "--out",
            out_sqpak.to_str().unwrap(),
            "--binary",
            out_bin.to_str().unwrap(),
            "--runtime",
            runtime_path.to_str().unwrap(),
            "--include-sourcemaps",
            "--name",
            "cli-test",
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "squark pack failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&out_bin).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "assembled binary should be executable");
    }

    // `squark inspect --dump-pack` on the appended binary (text and JSON).
    let dump_text = Command::new(squark_bin())
        .args(["inspect", "--dump-pack", out_bin.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        dump_text.status.success(),
        "squark inspect failed: {}",
        String::from_utf8_lossy(&dump_text.stderr)
    );
    let stdout = String::from_utf8_lossy(&dump_text.stdout);
    assert!(stdout.contains("appended runtime binary"));
    assert!(stdout.contains("/assets/main.js.map"));
    assert!(stdout.contains("external-stylesheet"));

    let dump_json = Command::new(squark_bin())
        .args([
            "inspect",
            "--dump-pack",
            out_sqpak.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(dump_json.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&dump_json.stdout).unwrap();
    assert_eq!(parsed["container"], "standalone-sqpak");
    assert_eq!(parsed["document"]["quirks_mode"], "standards");

    // Inspecting standalone document.bin also succeeds.
    let doc_info = inspect_path(&dir.join("basic.document.bin")).unwrap();
    assert_eq!(doc_info.container, ContainerKind::StandaloneDocumentBin);
    assert!(doc_info.document.is_some());

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn html_with_many_elements_rolls_over_multiple_journal_batches() {
    let mut html = String::from("<!DOCTYPE html><html><body>");
    for i in 0..2200 {
        html.push_str(&format!("<p data-i=\"{i}\">item {i}</p>"));
    }
    html.push_str("</body></html>");

    let compiled = compile_html_to_document_bin(&html, "/index.html").unwrap();
    assert!(
        compiled.batch_count >= 2,
        "expected rollover into >= 2 batches, got {}",
        compiled.batch_count
    );
    let doc = validate_document_bin(&compiled.bytes).unwrap();
    assert_eq!(doc.batches.len() as u32, compiled.batch_count);
    for batch in &doc.batches {
        assert!(batch.records.len() <= MAX_RECORD_BYTES / RECORD_SIZE);
    }
    // Tag `p` and attr `data-i` should each be defined once across all batches.
    let p_atoms = doc
        .atoms
        .values()
        .filter(|(k, u)| *k == AtomKind::Tag && String::from_utf16_lossy(u) == "p")
        .count();
    let attr_atoms = doc
        .atoms
        .values()
        .filter(|(k, u)| *k == AtomKind::Attribute && String::from_utf16_lossy(u) == "data-i")
        .count();
    assert_eq!(p_atoms, 1);
    assert_eq!(attr_atoms, 1);
}

#[test]
fn base_href_resolves_relative_scripts_and_stylesheets() {
    let html = r#"<!DOCTYPE html>
<html>
<head>
  <base href="/app/v2/">
  <link rel="stylesheet" href="theme.css">
  <script type="module" src="entry.js"></script>
  <script>console.log(1)</script>
</head>
<body></body>
</html>"#;
    let compiled = compile_html_to_document_bin(html, "/index.html").unwrap();
    let doc = validate_document_bin(&compiled.bytes).unwrap();
    assert_eq!(doc.scripts.len(), 3);
    assert_eq!(
        String::from_utf16_lossy(&doc.script_strings[doc.scripts[0].payload_str as usize]),
        "/app/v2/theme.css"
    );
    assert_eq!(
        String::from_utf16_lossy(&doc.script_strings[doc.scripts[1].payload_str as usize]),
        "/app/v2/entry.js"
    );
    assert_eq!(
        String::from_utf16_lossy(&doc.script_strings[doc.scripts[2].base_url_str as usize]),
        "/app/v2/"
    );
}

#[test]
fn malformed_packs_and_documents_are_rejected() {
    // 1. Writer rejects invalid and duplicate paths.
    let mut b = PackBuilder::new();
    assert!(
        b.add_entry("no-leading-slash", MimeSpec::Builtin(0), vec![])
            .is_err()
    );
    assert!(
        b.add_entry("/a/../b", MimeSpec::Builtin(0), vec![])
            .is_err()
    );
    assert!(b.add_entry("/a\\b", MimeSpec::Builtin(0), vec![]).is_err());
    b.add_entry("/a.txt", MimeSpec::Builtin(14), b"hi".to_vec())
        .unwrap();
    assert!(
        b.add_entry("/a.txt", MimeSpec::Builtin(14), b"dup".to_vec())
            .is_err()
    );
    b.add_entry("/b.txt", MimeSpec::Builtin(14), b"yo".to_vec())
        .unwrap();
    let valid_sqpak = b.build().unwrap();
    assert!(inspect_bytes(&valid_sqpak).is_ok());

    // 2. Truncated .sqpak header and truncated payload.
    assert!(inspect_bytes(&valid_sqpak[..16]).is_err());
    assert!(inspect_bytes(&valid_sqpak[..valid_sqpak.len() - 1]).is_err());

    // 3. Unknown pack magic / version.
    let mut bad_ver = valid_sqpak.clone();
    bad_ver[5] = 2;
    assert!(inspect_bytes(&bad_ver).is_err());

    // 4. Corrupt path hash in index entry 0.
    let mut bad_hash = valid_sqpak.clone();
    bad_hash[PACK_HEADER_SIZE] ^= 0xFF;
    assert!(inspect_bytes(&bad_hash).is_err());

    // 5. Unsorted index (swap entry 0 and entry 1).
    let mut swapped = valid_sqpak.clone();
    let e0 = valid_sqpak[32..64].to_vec();
    let e1 = valid_sqpak[64..96].to_vec();
    swapped[32..64].copy_from_slice(&e1);
    swapped[64..96].copy_from_slice(&e0);
    assert!(inspect_bytes(&swapped).is_err());

    // 6. Duplicate entry in index (overwrite entry 1 with entry 0).
    let mut dup_idx = valid_sqpak.clone();
    dup_idx[64..96].copy_from_slice(&e0);
    assert!(inspect_bytes(&dup_idx).is_err());

    // 7. Appended binary with bad CRC32 or bad trailer version.
    let appended = append_pack_to_binary(&fake_runtime_prefix(), &valid_sqpak).unwrap();
    assert!(inspect_bytes(&appended).is_ok());

    let mut bad_crc = appended.clone();
    let last = bad_crc.len() - 1;
    bad_crc[last] ^= 0x01;
    assert!(inspect_bytes(&bad_crc).is_err());

    let mut bad_trailer_ver = appended.clone();
    let ver_off = bad_trailer_ver.len() - TRAILER_SIZE + 8;
    bad_trailer_ver[ver_off..ver_off + 4].copy_from_slice(&99u32.to_le_bytes());
    assert!(inspect_bytes(&bad_trailer_ver).is_err());

    // 8. Truncated / bad-version document.bin.
    let doc = compile_html_to_document_bin("<!DOCTYPE html><p>ok</p>", "/index.html").unwrap();
    assert!(validate_document_bin(&doc.bytes).is_ok());
    assert!(validate_document_bin(&doc.bytes[..32]).is_err());
    let mut bad_doc_ver = doc.bytes.clone();
    bad_doc_ver[8..10].copy_from_slice(&2u16.to_le_bytes());
    assert!(validate_document_bin(&bad_doc_ver).is_err());
}

#[test]
fn all_150_r1_reftest_cases_compile_and_validate_through_squark_journal() {
    let r1_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/reftests/cases/r1");
    let mut count = 0usize;
    let mut stack = vec![r1_root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('_') {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("html") {
                let rel = path.strip_prefix(&r1_root).unwrap();
                let pack_path = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
                let html = fs::read_to_string(&path).unwrap();
                let compiled = compile_html_to_document_bin(&html, &pack_path)
                    .unwrap_or_else(|e| panic!("failed to compile {pack_path}: {e}"));
                let validated = validate_document_bin(&compiled.bytes)
                    .unwrap_or_else(|e| panic!("failed to validate {pack_path}: {e}"));
                assert_eq!(validated.header.quirks_mode, QuirksMode::Standards);
                count += 1;
            }
        }
    }
    assert_eq!(count, 150, "expected all 150 R1 reftest cases");
}

#[test]
fn a_windows_runtime_packs_to_an_exe_and_other_runtimes_keep_the_name() {
    let tmp = tempfile::tempdir().unwrap();
    let pack = |runtime: &[u8], name: &str| {
        let runtime_path = tmp.path().join(format!("{name}-runtime"));
        fs::write(&runtime_path, runtime).unwrap();
        pack_directory(&PackOptions {
            web_dir: fixtures_dir().join("basic-site"),
            html_path: None,
            out_sqpak: None,
            out_binary: Some(tmp.path().join(name)),
            runtime_binary: Some(runtime_path),
            include_sourcemaps: false,
            app_name: Some(name.to_owned()),
            skip_other_html: false,
            window: None,
            network_allow: Vec::new(),
            extra_assets: Vec::new(),
            native_modules: Vec::new(),
            prelude: None,
        })
        .unwrap()
        .output
        .unwrap()
    };

    let pe = [&b"MZ"[..], &fake_runtime_prefix()].concat();
    let windows = pack(&pe, "app");
    assert_eq!(windows.path, tmp.path().join("app.exe"));
    assert_eq!(windows.executable, windows.path);
    assert!(windows.path.is_file() && !tmp.path().join("app").exists());

    let named = pack(&pe, "Named.EXE");
    assert_eq!(named.path, tmp.path().join("Named.EXE"));

    let elf = [&b"\x7fELF"[..], &fake_runtime_prefix()].concat();
    let linux = pack(&elf, "linux-app");
    assert_eq!(linux.path, tmp.path().join("linux-app"));
}

#[test]
fn a_previous_macos_bundle_in_the_web_dir_is_never_packed_again() {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("dist");
    fs::create_dir_all(&web).unwrap();
    fs::copy(
        fixtures_dir().join("basic-site/index.html"),
        web.join("index.html"),
    )
    .unwrap();
    let pack = |name: &str| {
        pack_directory(&PackOptions {
            web_dir: web.clone(),
            html_path: None,
            out_sqpak: Some(tmp.path().join(name)),
            out_binary: None,
            runtime_binary: None,
            include_sourcemaps: false,
            app_name: Some("app".to_owned()),
            skip_other_html: false,
            window: None,
            network_allow: Vec::new(),
            extra_assets: Vec::new(),
            native_modules: Vec::new(),
            prelude: None,
        })
        .unwrap()
    };
    let before = pack("before.sqpak");

    // What an earlier `squark build` on macOS leaves in dist/.
    let contents = web.join("app.app/Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::create_dir_all(contents.join("Resources")).unwrap();
    fs::write(contents.join("Info.plist"), "<plist/>").unwrap();
    fs::write(contents.join("MacOS/app"), vec![0xcf; 4096]).unwrap();
    fs::write(contents.join("Resources/app.sqpak"), b"SQPK").unwrap();
    // A directory that only looks like a bundle is still packed.
    fs::create_dir_all(web.join("notes.app")).unwrap();
    fs::write(web.join("notes.app/readme.txt"), b"kept").unwrap();

    let after = pack("after.sqpak");
    assert_eq!(after.entry_count, before.entry_count + 1);
    let inspection = inspect_path(&tmp.path().join("after.sqpak")).unwrap();
    let json = serde_json::to_string(&inspection).unwrap();
    assert!(json.contains("/notes.app/readme.txt"));
    assert!(!json.contains("/app.app/"));
}
