#!/bin/sh
set -eu

script_dir="$(CDPATH= cd "$(dirname "$0")/.." && pwd)"
uninstaller="$script_dir/uninstall.sh"
postinstall="$script_dir/scripts/postinstall"
build_script="$script_dir/build-pkg.sh"

for version_bound_file in "$postinstall" "$uninstaller"; do
  grep -F 'format=%s\n' "$version_bound_file" >/dev/null || {
    echo "ownership marker is not package-versioned in $version_bound_file" >&2
    exit 1
  }
done
grep -F 's/@UNIONC_AGENT_PACKAGE_VERSION@/$VERSION/g' "$build_script" >/dev/null || {
  echo "package build does not bind lifecycle scripts to VERSION" >&2
  exit 1
}
grep -F '"state_dir": "/Library/Application Support/UnionC Agent"' "$build_script" >/dev/null || {
  echo "package build does not bind config.json to the macOS state directory" >&2
  exit 1
}

extract_function() {
  function_name="$1"
  source_file="$2"
  sed -n "/^${function_name}()/,/^}/p" "$source_file"
}

uninstall_functions="$(
  extract_function dscl_value "$uninstaller"
  extract_function listing_contains_id "$uninstaller"
  extract_function record_attribute_has_values "$uninstaller"
  extract_function text_contains_token "$uninstaller"
  extract_function record_attribute_contains_token "$uninstaller"
  extract_function group_is_in_use "$uninstaller"
)"

run_group_case() {
  mode="$1"
  expected="$2"
  case_program="$uninstall_functions
mode=\"\$1\"
guid='01234567-89AB-CDEF-0123-456789ABCDEF'
dscl() {
  operation=\"\$*\"
  case \"\$operation\" in
    '. -list /Users PrimaryGroupID')
      [ \"\$mode\" = list_failure ] && return 71
      if [ \"\$mode\" = primary_member ]; then
        printf 'alice 450\\n'
      else
        printf 'alice 999\\n'
      fi
      ;;
    '. -read /Groups/_unioncagent')
      [ \"\$mode\" = record_failure ] && return 72
      printf 'GeneratedUID: %s\\nPrimaryGroupID: 450\\n' \"\$guid\"
      case \"\$mode\" in
        supplementary_name) printf 'GroupMembership: alice\\n' ;;
        supplementary_uuid) printf 'GroupMembers: AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE\\n' ;;
        nested_child) printf 'NestedGroups: AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE\\n' ;;
        continued_member) printf 'GroupMembership:\\n alice\\n' ;;
      esac
      ;;
    '. -read /Groups/_unioncagent GeneratedUID')
      if [ \"\$mode\" = missing_guid ]; then
        printf 'GeneratedUID:\\n'
      else
        printf 'GeneratedUID: %s\\n' \"\$guid\"
      fi
      ;;
    '. -list /Groups')
      [ \"\$mode\" = group_list_failure ] && return 73
      printf '_unioncagent\\nadmin\\n'
      ;;
    '. -read /Groups/admin')
      [ \"\$mode\" = reference_read_failure ] && return 74
      printf 'PrimaryGroupID: 80\\n'
      case \"\$mode\" in
        nested_parent) printf 'NestedGroups: %s\\n' \"\$guid\" ;;
        uuid_parent) printf 'GroupMembers: %s\\n' \"\$guid\" ;;
        name_parent) printf 'GroupMembership: _unioncagent\\n' ;;
        continued_parent) printf 'NestedGroups:\\n %s\\n' \"\$guid\" ;;
      esac
      ;;
    *) return 75 ;;
  esac
}
group_is_in_use _unioncagent 450"
  set +e
  sh -c "$case_program" sh "$mode" >/dev/null 2>&1
  actual="$?"
  set -e
  if [ "$actual" -ne "$expected" ]; then
    echo "group safety case $mode: expected $expected, got $actual" >&2
    sh -x -c "$case_program" sh "$mode" >&2 || true
    exit 1
  fi
}

run_group_case unused 1
run_group_case primary_member 0
run_group_case supplementary_name 0
run_group_case supplementary_uuid 0
run_group_case nested_child 0
run_group_case continued_member 0
run_group_case nested_parent 0
run_group_case uuid_parent 0
run_group_case name_parent 0
run_group_case continued_parent 0
run_group_case list_failure 2
run_group_case record_failure 2
run_group_case missing_guid 2
run_group_case group_list_failure 2
run_group_case reference_read_failure 2

allocator_functions="$(
  extract_function listing_contains_id "$postinstall"
  extract_function next_free_id "$postinstall"
)"
set +e
sh -c "$allocator_functions
dscl() { return 71; }
next_free_id /Groups PrimaryGroupID" >/dev/null 2>&1
allocator_status="$?"
set -e
[ "$allocator_status" -ne 0 ] || {
  echo "ID allocator accepted a failed dscl enumeration" >&2
  exit 1
}

allocated="$(sh -c "$allocator_functions
dscl() {
  case \"\$*\" in
    '. -list /Groups PrimaryGroupID') printf '_first 450\\n' ;;
    '. -list /Users PrimaryGroupID') printf '_second 449\\n' ;;
    *) return 71 ;;
  esac
}
next_free_id /Groups PrimaryGroupID")"
[ "$allocated" = "448" ] || {
  echo "ID allocator expected 448 after collisions, got $allocated" >&2
  exit 1
}

binding_functions="$(
  extract_function existing_group_matches_marker "$postinstall"
  extract_function existing_user_matches_marker "$postinstall"
)"

run_group_binding_case() {
  case_name="$1"
  expected="$2"
  marker_owned="$3"
  marker_gid="$4"
  actual_gid="$5"
  actual_name="$6"
  set +e
  sh -c "$binding_functions
ownership_marker_present=\"\$1\"
group_created=\"\$1\"
created_group_gid=\"\$2\"
existing_group_matches_marker \"\$3\" \"\$4\"" sh \
    "$marker_owned" "$marker_gid" "$actual_gid" "$actual_name" >/dev/null 2>&1
  actual="$?"
  set -e
  if [ "$actual" -ne "$expected" ]; then
    echo "group identity binding case $case_name: expected $expected, got $actual" >&2
    exit 1
  fi
}

run_user_binding_case() {
  case_name="$1"
  expected="$2"
  marker_owned="$3"
  marker_uid="$4"
  marker_primary_gid="$5"
  actual_uid="$6"
  actual_primary_gid="$7"
  service_group_gid="$8"
  set +e
  sh -c "$binding_functions
ownership_marker_present=\"\$1\"
user_created=\"\$1\"
created_user_uid=\"\$2\"
created_user_primary_gid=\"\$3\"
existing_user_matches_marker \"\$4\" \"\$5\" 'UnionC Agent' \
  /usr/bin/false /var/empty 1 \"\$6\"" sh \
    "$marker_owned" "$marker_uid" "$marker_primary_gid" "$actual_uid" \
    "$actual_primary_gid" "$service_group_gid" >/dev/null 2>&1
  actual="$?"
  set -e
  if [ "$actual" -ne "$expected" ]; then
    echo "user identity binding case $case_name: expected $expected, got $actual" >&2
    exit 1
  fi
}

# A package-owned identity must stay bound to the exact numeric IDs recorded when it was
# created. A matching-name replacement with different numeric IDs must never receive secrets.
run_group_binding_case exact 0 1 450 450 'UnionC Agent'
run_group_binding_case replaced_gid 1 1 450 451 'UnionC Agent'
run_group_binding_case unowned_preexisting 1 0 - 451 'UnionC Agent'
run_group_binding_case wrong_name 1 1 450 450 'Different Account'

run_user_binding_case exact 0 1 450 450 450 450 450
run_user_binding_case replaced_uid 1 1 450 450 451 450 450
run_user_binding_case replaced_primary_gid 1 1 450 450 450 451 451
run_user_binding_case wrong_service_group 1 1 450 450 450 450 451
run_user_binding_case unowned_preexisting 1 0 - - 451 451 451
run_user_binding_case malformed_numeric_id 1 1 450 450 450:451 450 450

group_check_line="$(awk '/if ! existing_group_matches_marker / { print NR; exit }' "$postinstall")"
user_check_line="$(awk '/if ! existing_user_matches_marker / { print NR; exit }' "$postinstall")"
first_state_chown_line="$(awk '/^chown -R .*"\$state"/ { print NR; exit }' "$postinstall")"
case "$group_check_line:$user_check_line:$first_state_chown_line" in
  *[!0-9:]*)
    echo "Could not locate identity checks and state ownership change in postinstall" >&2
    exit 1
    ;;
esac
[ "$group_check_line" -lt "$first_state_chown_line" ] &&
  [ "$user_check_line" -lt "$first_state_chown_line" ] || {
  echo "postinstall changes state ownership before verifying account identity bindings" >&2
  exit 1
}

echo "macOS account safety tests: ok"
