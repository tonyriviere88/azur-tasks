//! Embeds `assets/app.ico` as the executable's icon, and the product's name as
//! its version resource.
//!
//! This writes the linker's `.res` input by hand instead of pulling in a crate
//! for it. The format is a flat list of length-prefixed records, MSVC `link`
//! accepts `.res` files directly, and doing it here means the build needs
//! neither `rc.exe` nor a build-dependency tree.
//!
//! The window's own icon is *not* here: `main` loads `assets/app-64.png` through
//! `eframe::icon_data`. This used to unpack the largest uncompressed image out of
//! the `.ico` instead, which stopped being possible when the artwork came from the
//! design system — every entry in that file is PNG-compressed, and a build script
//! with no PNG decoder cannot read one. Going through the PNG that generated the
//! entry is both simpler and one indirection shorter.

use std::{env, fs, path::PathBuf};

/// Resource types, from `winuser.h`.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;

/// `MOVEABLE | DISCARDABLE`, and `| PURE` for the directory. Ignored on 64-bit
/// Windows, but this is what `rc.exe` emits and there is no reason to differ.
const MEM_ICON: u16 = 0x1010;
const MEM_GROUP: u16 = 0x1030;
const MEM_VERSION: u16 = 0x0030;

const LANG_EN_US: u16 = 0x0409;

/// One image out of the `.ico`, with the directory fields that describe it.
struct Image {
    /// 0 means 256; the field is a single byte.
    width: u8,
    height: u8,
    colors: u8,
    planes: u16,
    bpp: u16,
    /// A whole PNG file, or a DIB (BITMAPINFOHEADER + pixels + mask). Copied into
    /// the resource either way: Windows has read PNG icon entries since Vista, and
    /// the design system ships all nine of them that way.
    data: Vec<u8>,
}

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");

    // Declare the GUI subsystem for a release build. The loader hands a console
    // only to `IMAGE_SUBSYSTEM_WINDOWS_CUI` images, so this is what stops a
    // console window from opening behind the app; `main.rs` carries the attribute
    // that does it, and only a build script can see which profile is being built.
    //
    // Cargo reports `release` for `--release` and for every profile that inherits
    // it (`--profile fast`), and `debug` for the dev profile -- exactly the split
    // wanted, since a dev build keeps its console so panics and logging stay
    // visible. Keyed on the profile rather than on `debug_assertions`, so enabling
    // assertions in a release profile cannot quietly bring the console back.
    println!("cargo::rustc-check-cfg=cfg(gui_subsystem)");
    if env::var("PROFILE").as_deref() == Ok("release") {
        println!("cargo::rustc-cfg=gui_subsystem");
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let ico = fs::read("assets/app.ico").expect("assets/app.ico is missing");
    let images = parse_ico(&ico);
    assert!(!images.is_empty(), "app.ico contains no images");

    // Only MSVC `link` takes a `.res`; other linkers would reject the argument
    // outright, so a non-MSVC build just goes without an embedded icon.
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let res = out.join("app.res");
        fs::write(&res, build_res(&images)).unwrap();
        println!(
            "cargo:rustc-link-arg-bin=azur-task-manager={}",
            res.display()
        );
    }
}

// ------------------------------------------------------------------ .ico ----

fn parse_ico(ico: &[u8]) -> Vec<Image> {
    assert!(ico.len() >= 6, "truncated .ico");
    assert_eq!(le16(ico, 2), 1, "not an icon file (resource type != 1)");
    let count = le16(ico, 4) as usize;
    assert!(ico.len() >= 6 + count * 16, "truncated .ico directory");

    (0..count)
        .map(|i| {
            let e = 6 + i * 16;
            let len = le32(ico, e + 8) as usize;
            let off = le32(ico, e + 12) as usize;
            assert!(
                off + len <= ico.len(),
                "image {i} runs past the end of the .ico"
            );
            Image {
                width: ico[e],
                height: ico[e + 1],
                colors: ico[e + 2],
                planes: le16(ico, e + 4),
                bpp: le16(ico, e + 6),
                data: ico[off..off + len].to_vec(),
            }
        })
        .collect()
}

// ------------------------------------------------------------------ .res ----

/// One `RT_ICON` per image, one `RT_GROUP_ICON` naming them, and one
/// `RT_VERSION` naming the product. Explorer picks the lowest-numbered group as
/// the file's icon, so the group gets id 1.
fn build_res(images: &[Image]) -> Vec<u8> {
    let mut res = Vec::new();

    // A `.res` opens with a 32-byte all-zero record. cvtres uses it to tell the
    // format apart from a COFF object.
    push(&mut res, 0, 0, 0, 0, &[]);

    for (i, img) in images.iter().enumerate() {
        push(&mut res, RT_ICON, id(i), MEM_ICON, LANG_EN_US, &img.data);
    }

    let mut dir = Vec::with_capacity(6 + images.len() * 14);
    dir.extend_from_slice(&0u16.to_le_bytes()); // reserved
    dir.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    dir.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (i, img) in images.iter().enumerate() {
        dir.push(img.width);
        dir.push(img.height);
        dir.push(img.colors);
        dir.push(0); // reserved
        dir.extend_from_slice(&img.planes.to_le_bytes());
        dir.extend_from_slice(&img.bpp.to_le_bytes());
        dir.extend_from_slice(&(img.data.len() as u32).to_le_bytes());
        // A group entry ends in the RT_ICON id, where the file's directory
        // holds a byte offset. This is the only difference between the two.
        dir.extend_from_slice(&id(i).to_le_bytes());
    }
    push(&mut res, RT_GROUP_ICON, 1, MEM_GROUP, LANG_EN_US, &dir);
    push(
        &mut res,
        RT_VERSION,
        1,
        MEM_VERSION,
        LANG_EN_US,
        &version_info(),
    );

    res
}

// -------------------------------------------------------------- version -----

/// The strings Windows reads to answer "what *is* this file?".
///
/// Without them, Explorer's Description column, the shell property sheet, the
/// real Task Manager's Name column -- and this program's own Name and Publisher
/// columns, which read `FileDescription` and `CompanyName` out of exactly this
/// resource -- all fall back to the file name. An executable with no version
/// resource is the one row in the list that cannot say what it is.
fn version_info() -> Vec<u8> {
    let num = |var: &str| {
        env::var(var)
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(0)
    };
    let (major, minor, patch) = (
        num("CARGO_PKG_VERSION_MAJOR"),
        num("CARGO_PKG_VERSION_MINOR"),
        num("CARGO_PKG_VERSION_PATCH"),
    );
    let version = format!("{major}.{minor}.{patch}.0");

    // `VS_FIXEDFILEINFO`: the same version again in binary, which is what the
    // property sheet and every installer read. A version is packed two `u16`s
    // to a `u32`, most significant pair first.
    let ms = (u32::from(major) << 16) | u32::from(minor);
    let ls = u32::from(patch) << 16;
    let mut fixed = Vec::with_capacity(52);
    for field in [
        0xFEEF_04BDu32, // dwSignature
        0x0001_0000,    // dwStrucVersion: this layout is 1.0
        ms,
        ls, // dwFileVersion
        ms,
        ls,          // dwProductVersion
        0x3F,        // dwFileFlagsMask: all six flags are meaningful
        0,           // dwFileFlags: not a debug, patched or prerelease build
        0x0004_0004, // dwFileOS: VOS_NT | VOS__WINDOWS32
        1,           // dwFileType: VFT_APP
        0,           // dwFileSubtype: none, for an application
        0,
        0, // dwFileDate, which nothing has ever written
    ] {
        fixed.extend_from_slice(&field.to_le_bytes());
    }

    let strings = Node::branch(
        "StringFileInfo",
        // US English (0409) in Unicode (04B0 = codepage 1200). `read_version_info`
        // in `sys::meta` looks the pair up through `\VarFileInfo\Translation`
        // rather than assuming it, which is why the `Var` below is not optional.
        vec![Node::branch(
            "040904B0",
            vec![
                // The product family, not a company: this is a personal tool and
                // the field is the only place a publisher can be stated at all.
                Node::string("CompanyName", "Azur"),
                Node::string("FileDescription", "Azur Task Manager"),
                Node::string("FileVersion", &version),
                Node::string("InternalName", "azur-task-manager"),
                Node::string("OriginalFilename", "azur-task-manager.exe"),
                Node::string("ProductName", "Azur Task Manager"),
                Node::string("ProductVersion", &version),
            ],
        )],
    );

    let var = Node::branch(
        "VarFileInfo",
        vec![Node::binary(
            "Translation",
            [0x0409u16, 0x04B0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        )],
    );

    let mut out = Vec::new();
    Node {
        key: "VS_VERSION_INFO",
        text: false,
        value: fixed,
        value_len: 52,
        children: vec![strings, var],
    }
    .emit(&mut out);
    out
}

/// A node in the `VS_VERSIONINFO` tree.
///
/// Every node is `wLength, wValueLength, wType`, a NUL-terminated UTF-16 key,
/// then its value, then its children -- with each of those members padded out to
/// a 4-byte boundary. `wValueLength` counts *characters* for a text node and
/// *bytes* for a binary one, which is the format's one real trap: the same field
/// means two different things four bytes apart in the same tree.
struct Node {
    key: &'static str,
    text: bool,
    value: Vec<u8>,
    value_len: u16,
    children: Vec<Node>,
}

impl Node {
    /// A node that only holds other nodes.
    fn branch(key: &'static str, children: Vec<Node>) -> Self {
        Self {
            key,
            text: true,
            value: Vec::new(),
            value_len: 0,
            children,
        }
    }

    /// A `key=value` string. Its length includes the terminating NUL.
    fn string(key: &'static str, value: &str) -> Self {
        let value = utf16z(value);
        let value_len = (value.len() / 2) as u16;
        Self {
            key,
            text: true,
            value,
            value_len,
            children: Vec::new(),
        }
    }

    fn binary(key: &'static str, value: Vec<u8>) -> Self {
        let value_len = value.len() as u16;
        Self {
            key,
            text: false,
            value,
            value_len,
            children: Vec::new(),
        }
    }

    fn emit(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(&0u16.to_le_bytes()); // wLength, patched below
        out.extend_from_slice(&self.value_len.to_le_bytes());
        out.extend_from_slice(&u16::from(self.text).to_le_bytes());
        out.extend_from_slice(&utf16z(self.key));
        align4(out);
        out.extend_from_slice(&self.value);
        align4(out);
        for child in &self.children {
            child.emit(out);
            align4(out);
        }
        // The trailing padding is counted in. A reader that rounds `wLength` up
        // to a 4-byte boundary before walking on -- which is what Windows does --
        // gets the same answer either way, and one that does not gets a right
        // answer instead of a truncated one.
        let len = (out.len() - start) as u16;
        out[start..start + 2].copy_from_slice(&len.to_le_bytes());
    }
}

/// Every member of a version node starts on a 4-byte boundary. The resource is
/// built from offset zero, so aligning the buffer aligns the member.
fn align4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn id(index: usize) -> u16 {
    index as u16 + 1
}

/// `DataSize, HeaderSize, Type, Name, DataVersion, MemoryFlags, LanguageId,
/// Version, Characteristics`, then the data. Both ids are ordinals, written as
/// `0xFFFF` followed by the number, which fixes the header at 32 bytes. Records
/// are padded to a 4-byte boundary.
fn push(res: &mut Vec<u8>, ty: u16, name: u16, mem: u16, lang: u16, data: &[u8]) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&ty.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&name.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    res.extend_from_slice(&mem.to_le_bytes());
    res.extend_from_slice(&lang.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // Version
    res.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    res.extend_from_slice(data);
    while res.len() % 4 != 0 {
        res.push(0);
    }
}

// ---------------------------------------------------------------- helpers ---

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
