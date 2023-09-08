fn main() {
    let mut dest = cmake::build("wirehair");
    dest.push("build");
    println!("cargo:rustc-link-search=native={}", dest.display());
    println!("cargo:rustc-link-lib=static=wirehair");
    println!("cargo:rustc-link-lib=dylib=stdc++");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=wirehair");
}
