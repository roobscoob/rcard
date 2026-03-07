# Build the Hubris image and run it in Renode
def main [
    --skip-build  # Skip the build step and just run Renode
    --with: string  # Path to a pre-built sdmmc.img (skips build, uses this image directly)
] {
    let project = ($env.FILE_PWD)
    let zip = ($project | path join rcard-build.zip)
    let elf = ($project | path join build img final.elf)
    source ./scripts/sdmmc.nu
    let build_dir = ($project | path join build)

    # make build_dir if it doesn't exist, to avoid errors from later steps
    if not ($build_dir | path exists) { mkdir $build_dir }

    let $fw = if $with != null {
        $with
    } else if not $skip_build {
        ensure-hubake
        fix-hubake-cache
        fix-lld-linker
        setup-arm-cc

        let app_kdl = ($project | path join .work app.kdl)
        python ($project | path join scripts kdl-preprocess.py) ($project | path join app.kdl) $app_kdl
        hubake build $app_kdl

        # Extract ELFs from the build archive
        mkdir ($build_dir | path join img)
        mkdir ($build_dir | path join elf)
        python -c (["import zipfile; z=zipfile.ZipFile(r'" $zip "'); [z.extract(n, r'" $build_dir "') for n in z.namelist() if n.startswith('elf/') or n.startswith('img/')]"] | str join)

        # Extract raw binary from ELF (LOAD segments only)
        let bin_path = ($build_dir | path join img final.bin)
        rust-objcopy -O binary ($build_dir | path join img final.elf) $bin_path

        # Pack the firmware binary into the sdmmc image
        sdmmc pack firmware $bin_path

        # if there is a sdmmc directory...
        let sdmmc_dir = ($project | path join sdmmc)
        if ($sdmmc_dir | path exists) {
            # then iterate over the block devices
            let items = sdmmc layout | get block_devices | get sdmmc | where {|v| $v.format == "littlefs" }

            for item in $items {
                let name = ($item | get name)
                let path = ($sdmmc_dir | path join $name)
                if ($path | path exists) {
                    print $"Formatting partition '($name)' with contents of ($path)"
                    sdmmc format littlefs $name --with $path
                } else {
                    print $"No directory found for partition '($name)' at ($path), skipping."
                }
            }
        }        

        # Use the built image for running in Renode
        ($build_dir | path join sdmmc.img)
    }

    # Copy sdmmc image to .state/ so runtime mutations don't affect the build
    let state_dir = ($project | path join .state)
    if not ($state_dir | path exists) { mkdir $state_dir }
    let state_img = ($state_dir | path join sdmmc.img)
    cp $fw $state_img

    rm -rf ($build_dir | path join boot.bin)
    sdmmc open firmware --img $state_img | save ($build_dir | path join boot.bin)

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

    $env.RCARD_SDMMC_IMG = $state_img
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

# Find the ARM GNU Toolchain and set CC/AR env vars for cross-compilation.
# Searches PATH first, then common install locations.
def --env setup-arm-cc [] {
    let gcc = (which arm-none-eabi-gcc | get -o 0.path)
    let bin_dir = if $gcc != null {
        $gcc | path dirname
    } else {
        # Search common install locations
        let search_dirs = [
            "C:/Program Files (x86)"
            "C:/Program Files"
            $"($env.USERPROFILE)/scoop/apps"
        ]
        let bin_dir = ($search_dirs
            | where { $in | path exists }
            | each { |dir| ls $dir | where name =~ "arm-none-eabi|gcc-arm-none-eabi" | get name }
            | flatten
            | each { |dir| ls $dir | get name }
            | flatten
            | each { |dir| $dir | path join bin }
            | where { ($in | path join arm-none-eabi-gcc.exe) | path exists }
            | get -o 0)
        if $bin_dir == null {
            error make { msg: "arm-none-eabi-gcc not found. Install the Arm GNU Toolchain and ensure it is on PATH." }
        }
        $bin_dir
    }

    $env.CC_thumbv8m_main_none_eabihf = ($bin_dir | path join arm-none-eabi-gcc)
    $env.AR_thumbv8m_main_none_eabihf = ($bin_dir | path join arm-none-eabi-ar)

    # Tell bindgen's clang this is a freestanding (no OS) target so it uses
    # its built-in stdint.h instead of trying to find a system one.
    $env.BINDGEN_EXTRA_CLANG_ARGS = "-ffreestanding"
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
