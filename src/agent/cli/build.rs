use std::fs;
use std::path::Path;

fn main() {
    let samples_dir = Path::new("product/agent_runtime/src/skills/assets/samples");
    if samples_dir.exists() {
        println!("cargo:rerun-if-changed={}", samples_dir.display());
        visit_dir(samples_dir);
    }

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_manifest_file("product/windows_sandbox/lha-windows-sandbox-setup.manifest");
        let _ = res.compile();
    }
}

fn visit_dir(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            visit_dir(&path);
        }
    }
}
