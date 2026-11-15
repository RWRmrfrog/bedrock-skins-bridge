//! Compares the PNG decoder with Pillow: `cargo run --example pngcheck DIR NAME...`, where DIR
//! holds NAME.png and Pillow's RGBA conversion NAME.rgba.
fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("dir"));
    for name in args {
        let png = std::fs::read(dir.join(format!("{name}.png"))).unwrap();
        let want = std::fs::read(dir.join(format!("{name}.rgba"))).unwrap();
        match bedrock_skins_core::codec::png::decode(&png) {
            Ok(img) => println!("{name}: {}", if img.pixels == want { "identical" } else { "DIFFERENT" }),
            Err(e) => println!("{name}: error {e}"),
        }
    }
}
