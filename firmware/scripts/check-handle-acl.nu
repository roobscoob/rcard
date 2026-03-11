# Post-build handle ACL checker.
#
# Verifies that every server receiving handles via #[handle(move)] or
# #[handle(clone)] has the necessary IPC permissions to use those handles.
#
# Reads:
#   .work/ipc_meta/resource.*.json  - emitted by #[resource] / #[interface]
#   .work/ipc_meta/server.*.json    - emitted by server!
#   .work/app.uses.json             - task dependency declarations
#   .work/app.peers.json            - bidirectional peer relationships

def main [project: path] {
    let work = ($project | path join .work)
    let meta_dir = ($work | path join ipc_meta)

    if not ($meta_dir | path exists) {
        return
    }

    let all_files = (ls $meta_dir | each {|e| ($meta_dir | path join ($e | get name))} | where {|e| ($e | path parse).extension == "json"})
    let resource_files = ($all_files | where {|v| ($v | path basename) starts-with "resource." })
    let server_files = ($all_files | where {|v| ($v | path basename) starts-with "server." })

    if ($resource_files | is-empty) or ($server_files | is-empty) {
        return
    }

    let resources = ($resource_files | each {|v| open $v })
    let servers = ($server_files | each {|v| open $v })

    # Validate that every server task emitted metadata.
    let uses = if (($work | path join app.uses.json) | path exists) {
        open ($work | path join app.uses.json)
    } else {
        {}
    }

    let known_tasks = ($uses | columns)
    let server_tasks = ($servers | get task)
    mut did_error = false
    for task in $known_tasks {
        let task_uses = ($uses | get $task)
        for dep in $task_uses {
            if $dep not-in $server_tasks and $dep != "sysmodule_time" and $dep != "sysmodule_usart" {
                print $"  (ansi red)⚠(ansi reset) task ($task) uses ($dep), but no server metadata found for ($dep)"
                $did_error = true
            }
        }
    }

    if $did_error {
        print ""
        error make { msg: "handle ACL check failed due to missing server metadata" }
    }

    let peers_data = if (($work | path join app.peers.json) | path exists) {
        open ($work | path join app.peers.json)
    } else {
        {}
    }

    # Build: trait_name -> [server_task, ...]
    mut trait_to_tasks = {}
    for server in $servers {
        for t in $server.serves {
            let existing = ($trait_to_tasks | get -o $t | default [])
            $trait_to_tasks = ($trait_to_tasks | upsert $t ($existing | append $server.task))
        }
    }

    # Build: interface_name -> [concrete_trait_name, ...]
    let interface_names = ($resources | where is_interface == true | get trait_name)
    mut iface_to_impls = {}
    for res in ($resources | where {|r| $r.implements != null }) {
        let iface = $res.implements
        let existing = ($iface_to_impls | get -o $iface | default [])
        $iface_to_impls = ($iface_to_impls | upsert $iface ($existing | append $res.trait_name))
    }

    # Check each resource's handle params
    mut errors = []
    for res in ($resources | where is_interface == false) {
        let host_tasks = ($trait_to_tasks | get -o $res.trait_name | default [])
        if ($host_tasks | is-empty) { continue }

        for param in $res.handle_params {
            # Skip concrete (same-server) handles
            if $param.handle_trait == "(concrete)" { continue }

            let handle_trait = $param.handle_trait

            # Resolve handle_trait to all possible server tasks
            mut provider_tasks = []

            if $handle_trait in $interface_names {
                # Interface: collect all implementors' tasks
                let impls = ($iface_to_impls | get -o $handle_trait | default [])
                for impl_trait in $impls {
                    let tasks = ($trait_to_tasks | get -o $impl_trait | default [])
                    $provider_tasks = ($provider_tasks | append $tasks)
                }
                # Also include direct servers of the interface trait itself
                let direct = ($trait_to_tasks | get -o $handle_trait | default [])
                $provider_tasks = ($provider_tasks | append $direct)
            } else {
                # Concrete resource
                $provider_tasks = ($trait_to_tasks | get -o $handle_trait | default [])
            }

            $provider_tasks = ($provider_tasks | uniq)

            if ($provider_tasks | is-empty) {
                $errors = ($errors | append (
                    $"warning: ($res.trait_name)::($param.method) accepts #[handle\(($param.mode)\)] impl ($handle_trait), but no server serves ($handle_trait)"
                ))
                continue
            }

            for host in $host_tasks {
                for provider in $provider_tasks {
                    # Same server = always ok (co-located)
                    if $host == $provider { continue }

                    if $param.mode == "clone" {
                        # Clone peers must be co-located (same server! invocation).
                        # If we reach here, they're on different servers.
                        $errors = ($errors | append (
                            [
                                $"($res.trait_name)::($param.method) takes #[handle\(clone\)] impl ($handle_trait)"
                                $"  ($res.trait_name) is served by ($host)"
                                $"  ($handle_trait) is served by ($provider)"
                                $"  clone peers must be co-located in the same server! block"
                            ] | str join "\n"
                        ))
                    } else {
                        # Move handles: hosting server needs IPC access to provider
                        if not (has-access $host $provider $uses $peers_data) {
                            let short = ($provider | str replace "sysmodule_" "")
                            $errors = ($errors | append (
                                [
                                    $"($res.trait_name)::($param.method) takes #[handle\(move\)] impl ($handle_trait)"
                                    $"  ($res.trait_name) is served by ($host)"
                                    $"  ($handle_trait) can come from ($provider)"
                                    $"  but ($host) does not have IPC access to ($provider)"
                                    $"  -> add `uses-sysmodule \"($short)\"` or peer ($host) with ($provider) in app.kdl"
                                ] | str join "\n"
                            ))
                        }
                    }
                }
            }
        }
    }

    if ($errors | is-not-empty) {
        print ""
        for err in $errors {
            print $"  (ansi red)✗(ansi reset) ($err)"
            print ""
        }
        let n = ($errors | length)
        let msg = $"handle ACL check failed with ($n) violations"
        error make { msg: $msg }
    }
}

def has-access [from: string, to: string, uses: record, peers_data: record] {
    # Check uses: does 'from' list 'to' as a dependency?
    let from_uses = ($uses | get -o $from | default [])
    if $to in $from_uses { return true }

    # Check peers: bidirectional
    let from_peers = ($peers_data | get -o $from | default [])
    if $to in $from_peers { return true }

    let to_peers = ($peers_data | get -o $to | default [])
    if $from in $to_peers { return true }

    false
}
