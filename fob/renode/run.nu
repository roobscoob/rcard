def main [bin: path] {
    let bin = $bin | str replace --all '\' '/'
    $env.DEFMT_ELF = $bin
    $env.DEFMT_PRINT = (which defmt-print | get 0.path)
    renode --console -e $"set bin \"($bin)\"" -e "include @renode/sf32lb52.resc"
}
