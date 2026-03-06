# Build the Hubris image and run it in Renode
def main [
    --skip-build  # Skip the build step and just run Renode
] {
    let project = ($env.FILE_PWD)
    let zip = ($project | path join rcard-build.zip)
    let elf = ($project | path join build img final.elf)

    if not $skip_build {
        ensure-hubake
        fix-hubake-cache
        fix-lld-linker

        let app_kdl = ($project | path join .app.kdl.out)
        python ($project | path join scripts kdl-preprocess.py) ($project | path join app.kdl) $app_kdl
        hubake build $app_kdl

        # Extract ELFs from the build archive
        let build_dir = ($project | path join build)
        mkdir ($build_dir | path join img)
        mkdir ($build_dir | path join elf)
        python -c (["import zipfile; z=zipfile.ZipFile(r'" $zip "'); [z.extract(n, r'" $build_dir "') for n in z.namelist() if n.startswith('elf/') or n.startswith('img/')]"] | str join)
    }

    if not ($elf | path exists) {
        error make { msg: $"ELF not found at ($elf). Run without --skip-build first." }
    }

    let bin = ($elf | str replace --all '\' '/')
    let kernel_elf = ($project | path join build elf kernel | str replace --all '\' '/')
    let resc = ($project | path join renode sf32lb52.resc | str replace --all '\' '/')

    ensure-rust-objdump

    # Find die_impl's post-epitaph infinite loop by disassembling the kernel.
    # This is the last `b .` (self-branch) in die_impl, reached after
    # KERNEL_EPITAPH has been fully written.
    let die_loop = (python ($project | path join renode find_die_loop.py) ($project | path join build elf kernel) | str trim)

    mkdir .state
    $env.RCARD_SDCARD_IMG = ($project | path join .state sdcard.img)
    renode --console -e $"set bin \"($bin)\"; set kernel_elf \"($kernel_elf)\"; set die_loop ($die_loop); set resc \"($resc)\"" -e "include $resc"
}

# Install cargo-binutils (provides rust-objdump) if not already present
def ensure-rust-objdump [] {
    if (which rust-objdump | is-empty) {
        print "rust-objdump not found, installing cargo-binutils..."
        cargo install cargo-binutils
    }
}

# Install hubake if not already present
def ensure-hubake [] {
    if (which hubake | is-empty) {
        print "hubake not found, installing..."
        cargo install hubake --git "https://github.com/cbiffle/exhubris" --rev "69d2f5ca8017fc3aaf692eae9455ac9fcd883173"
    }
}

# Workaround: rustc looks for ld.lld (no extension) but Windows only has
# ld.lld.exe. Create a hardlink so the linker is found.
def fix-lld-linker [] {
    let toolchain = (rustup show active-toolchain | split row ' ' | first)
    let exe = ([$env.USERPROFILE .rustup toolchains $toolchain lib rustlib x86_64-pc-windows-msvc bin gcc-ld ld.lld.exe] | path join)
    let noext = ($exe | str replace '.exe' '')

    if ($exe | path exists) and not ($noext | path exists) {
        ^powershell -Command $"New-Item -ItemType HardLink -Path '($noext)' -Target '($exe)' | Out-Null"
    }
}

# Ensure hubris-build hardlink exists alongside hubris-build.exe
def fix-hubake-cache [] {
    # hubake caches at: %APPDATA%/hubris/git/<base64(repo)>/<rev>/bin/hubris-build
    # but cargo install only creates hubris-build.exe, so the cache check fails
    let repo = "https://github.com/cbiffle/exhubris"
    let rev = "69d2f5ca8017fc3aaf692eae9455ac9fcd883173"
    let repo_b64 = (python -c $"import base64; print\(base64.b64encode\(b'($repo)'\).decode\(\)\)")
    let exe = ([$env.APPDATA hubris git $repo_b64 $rev bin hubris-build.exe] | path join)
    let noext = ($exe | str replace '.exe' '')

    if ($exe | path exists) and not ($noext | path exists) {
        ^powershell -Command $"New-Item -ItemType HardLink -Path '($noext)' -Target '($exe)' | Out-Null"
    }
}
