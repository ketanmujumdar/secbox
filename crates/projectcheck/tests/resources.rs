use std::fs;

use projectcheck::resources::{
    load, parse_config, resolve, ByteSize, Clamped, ConfigError, HostResources, MilliCpus,
    ResourceError, ResourceRequest, UserLimits, DEFAULT_CPUS,
};
use tempfile::TempDir;

const GIB: u64 = 1 << 30;

fn size(text: &str) -> ByteSize {
    text.parse().unwrap()
}

fn limits() -> UserLimits {
    UserLimits {
        max_memory: size("8G"),
        max_cpus: "4".parse().unwrap(),
        max_disk: size("50G"),
        max_build_memory: Some(size("12G")),
    }
}

fn big_host() -> HostResources {
    HostResources {
        available_memory: 32 * GIB,
        memory_reserve: 4 * GIB,
        available_millicpus: 10_000,
        cpu_reserve_millis: 2_000,
        available_disk: 500 * GIB,
        disk_reserve: 20 * GIB,
    }
}

#[test]
fn parses_the_documented_block() {
    let request = parse_config(
        "resources:\n  memory: 6G\n  cpus: 4\n  disk: 30G\n  build:\n    memory: 8G\n",
    )
    .unwrap();
    assert_eq!(
        request,
        ResourceRequest {
            memory: Some(size("6G")),
            cpus: Some("4".parse().unwrap()),
            disk: Some(size("30G")),
            build_memory: Some(size("8G")),
        }
    );
    assert_eq!(request.memory.unwrap().bytes(), 6 * GIB);
}

#[test]
fn sizes_parse() {
    assert_eq!(size("512M").bytes(), 512 << 20);
    assert_eq!(size("6g").bytes(), 6 * GIB);
    assert_eq!(size("6GiB").bytes(), 6 * GIB);
    assert_eq!(size("1.5G").bytes(), 3 * GIB / 2);
    assert_eq!(size("1T").bytes(), 1 << 40);
    assert_eq!(size("1024B").bytes(), 1024);
    assert_eq!(size("6G").to_string(), "6G");
}

#[test]
fn bad_sizes_are_rejected() {
    for text in [
        "",
        "G",
        "6",
        "0",
        "0G",
        "0.0G",
        "-1G",
        "+1G",
        "6 G",
        " 6G",
        "6G ",
        "1e3G",
        "0x10G",
        "1.G",
        ".5G",
        "1..5G",
        "1.5.2G",
        "6X",
        "6GG",
        "\u{0663}G",
        "0.0000001K",
        "18446744073709551616B",
        "17179869184G",
        "99999999999999999999999999T",
        "1G1G",
    ] {
        assert!(text.parse::<ByteSize>().is_err(), "accepted {text:?}");
    }
    assert!(format!("{}G", "9".repeat(5000))
        .parse::<ByteSize>()
        .is_err());
}

#[test]
fn largest_size_fits_and_one_more_overflows() {
    assert_eq!(size("18446744073709551615B").bytes(), u64::MAX);
    assert!("16777216T".parse::<ByteSize>().is_err());
}

#[test]
fn cpus_parse() {
    let parse = |text: &str| text.parse::<MilliCpus>().unwrap().millis();
    assert_eq!(parse("4"), 4000);
    assert_eq!(parse("0.5"), 500);
    assert_eq!(parse("1.25"), 1250);
    assert_eq!(parse("0.001"), 1);
}

#[test]
fn bad_cpus_are_rejected() {
    for text in [
        "",
        "0",
        "0.0",
        "-1",
        "+1",
        "1.2345",
        "abc",
        "1e3",
        "4097",
        "1 ",
        "99999999999",
    ] {
        assert!(text.parse::<MilliCpus>().is_err(), "accepted {text:?}");
    }
}

fn config_error(text: &str) -> ConfigError {
    parse_config(text).expect_err(text)
}

#[test]
fn bad_values_in_config_are_rejected() {
    for text in [
        "resources: {memory: 6}",
        "resources: {memory: 0G}",
        "resources: {memory: -1G}",
        "resources: {memory: true}",
        "resources: {memory: null}",
        "resources: {memory: [6G]}",
        "resources: {memory: !custom 6G}",
        "resources: {cpus: 0}",
        "resources: {cpus: -1}",
        "resources: {cpus: .inf}",
        "resources: {cpus: .nan}",
        "resources: {cpus: 1.0e400}",
        "resources: {cpus: 99999999999999999999}",
        "resources: {cpus: yes}",
        "resources: {cpus: !custom 2}",
        "resources: {disk: 18446744073709551616B}",
        "resources: {build: {memory: 0M}}",
        "resources: {build: 8G}",
        "resources: 5",
        "resources: null",
        "- resources",
    ] {
        assert!(parse_config(text).is_err(), "accepted {text}");
    }
}

#[test]
fn unknown_keys_are_errors_at_every_level() {
    for text in [
        "other: 1",
        "resources: {memory: 1G, swap: 1G}",
        "resources: {build: {memory: 1G, cpus: 2}}",
        "resources: {Memory: 1G}",
        "? [a, b]\n: 1\n",
    ] {
        assert!(matches!(
            config_error(text),
            ConfigError::Resource(ResourceError::UnknownKey { .. })
        ));
    }
}

#[test]
fn an_empty_file_requests_nothing() {
    assert_eq!(parse_config("").unwrap(), ResourceRequest::default());
    assert_eq!(
        parse_config("# a comment\n").unwrap(),
        ResourceRequest::default()
    );
}

#[test]
fn duplicate_keys_are_errors() {
    assert!(parse_config("resources: {memory: 1G, memory: 64G}").is_err());
}

#[test]
fn alias_bomb_in_config_is_an_error() {
    let mut text = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for level in 1..12 {
        let prev = level - 1;
        text.push_str(&format!(
            "a{level}: &a{level} [*a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}]\n"
        ));
    }
    assert!(parse_config(&text).is_err());
}

#[test]
fn oversized_config_is_an_error() {
    let text = format!("# {}\n", "x".repeat(70 * 1024));
    assert!(parse_config(&text).is_err());
}

#[test]
fn load_handles_missing_and_present_files() {
    let dir = TempDir::new().unwrap();
    assert_eq!(load(dir.path()).unwrap(), ResourceRequest::default());
    fs::write(dir.path().join(brand::CONFIG_FILE), "resources: {cpus: 2}").unwrap();
    assert_eq!(load(dir.path()).unwrap().cpus.unwrap().millis(), 2000);
}

#[cfg(unix)]
#[test]
fn load_refuses_a_link_out_of_the_project_or_to_an_env_file() {
    use std::os::unix::fs::symlink;
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("target"), "resources: {cpus: 2}").unwrap();

    let dir = TempDir::new().unwrap();
    symlink(
        outside.path().join("target"),
        dir.path().join(brand::CONFIG_FILE),
    )
    .unwrap();
    assert!(matches!(
        load(dir.path()),
        Err(ConfigError::File(projectcheck::Error::PathEscapes(_)))
    ));

    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    symlink(dir.path().join(".env"), dir.path().join(brand::CONFIG_FILE)).unwrap();
    assert!(matches!(
        load(dir.path()),
        Err(ConfigError::File(projectcheck::Error::EnvFileRefused(_)))
    ));
}

#[test]
fn requests_within_limits_are_kept() {
    let request =
        parse_config("resources: {memory: 6G, cpus: 2, disk: 30G, build: {memory: 8G}}").unwrap();
    let out = resolve(&request, &limits(), &big_host()).unwrap();
    assert_eq!(out.memory, size("6G"));
    assert_eq!(out.cpus.millis(), 2000);
    assert_eq!(out.disk, size("30G"));
    assert_eq!(out.build_memory, size("8G"));
    assert!(out.clamped.is_empty());
}

#[test]
fn hostile_requests_are_clamped_to_the_user_maximum() {
    let request =
        parse_config("resources: {memory: 1024T, cpus: 4096, disk: 1024T, build: {memory: 1024T}}")
            .unwrap();
    let out = resolve(&request, &limits(), &big_host()).unwrap();
    assert_eq!(out.memory, size("8G"));
    assert_eq!(out.cpus.millis(), 4000);
    assert_eq!(out.disk, size("50G"));
    assert_eq!(out.build_memory, size("12G"));
    assert_eq!(
        out.clamped,
        vec![
            Clamped::Memory,
            Clamped::Cpus,
            Clamped::Disk,
            Clamped::BuildMemory
        ]
    );
}

#[test]
fn absent_requests_use_defaults_within_the_maximum() {
    let out = resolve(&ResourceRequest::default(), &limits(), &big_host()).unwrap();
    assert_eq!(out.cpus, DEFAULT_CPUS);
    assert_eq!(out.memory, out.build_memory);
    assert!(out.clamped.is_empty());

    let small = UserLimits {
        max_memory: size("1G"),
        max_cpus: "1".parse().unwrap(),
        max_disk: size("2G"),
        max_build_memory: None,
    };
    let out = resolve(&ResourceRequest::default(), &small, &big_host()).unwrap();
    assert_eq!(out.memory, size("1G"));
    assert_eq!(out.cpus.millis(), 1000);
    assert_eq!(out.disk, size("2G"));
    assert!(out.clamped.is_empty(), "defaults are not repo requests");
}

#[test]
fn build_memory_without_its_own_maximum_uses_the_memory_maximum() {
    let request = ResourceRequest {
        build_memory: Some(size("100G")),
        ..ResourceRequest::default()
    };
    let user = UserLimits {
        max_build_memory: None,
        ..limits()
    };
    assert_eq!(
        resolve(&request, &user, &big_host()).unwrap().build_memory,
        size("8G")
    );
}

fn memory_host(available_gib: u64, reserve_gib: u64) -> HostResources {
    HostResources {
        available_memory: available_gib * GIB,
        memory_reserve: reserve_gib * GIB,
        ..big_host()
    }
}

#[test]
fn memory_reserve_boundary() {
    let request = ResourceRequest {
        memory: Some(size("6G")),
        ..ResourceRequest::default()
    };
    assert!(resolve(&request, &limits(), &memory_host(10, 4)).is_ok());
    let err = resolve(&request, &limits(), &memory_host(9, 4)).unwrap_err();
    assert!(matches!(
        err,
        ResourceError::HostReserve {
            resource: "memory",
            ..
        }
    ));
    assert!(err.to_string().contains("6G"));
}

#[test]
fn reserve_above_available_is_an_error_and_does_not_underflow() {
    let request = ResourceRequest {
        memory: Some(size("1G")),
        ..ResourceRequest::default()
    };
    assert!(resolve(&request, &limits(), &memory_host(1, 8)).is_err());
}

#[test]
fn build_memory_counts_toward_the_reserve() {
    let request = ResourceRequest {
        memory: Some(size("2G")),
        build_memory: Some(size("10G")),
        ..ResourceRequest::default()
    };
    assert!(matches!(
        resolve(&request, &limits(), &memory_host(12, 4)),
        Err(ResourceError::HostReserve { .. })
    ));
}

#[test]
fn cpu_and_disk_reserves_are_enforced() {
    let request = ResourceRequest {
        cpus: Some("4".parse().unwrap()),
        ..ResourceRequest::default()
    };
    let host = HostResources {
        available_millicpus: 5000,
        cpu_reserve_millis: 2000,
        ..big_host()
    };
    assert!(matches!(
        resolve(&request, &limits(), &host),
        Err(ResourceError::HostReserve {
            resource: "CPU",
            ..
        })
    ));

    let request = ResourceRequest {
        disk: Some(size("30G")),
        ..ResourceRequest::default()
    };
    let host = HostResources {
        available_disk: 40 * GIB,
        disk_reserve: 20 * GIB,
        ..big_host()
    };
    assert!(matches!(
        resolve(&request, &limits(), &host),
        Err(ResourceError::HostReserve {
            resource: "disk",
            ..
        })
    ));
}

#[test]
fn zero_cannot_be_built_directly() {
    assert!(ByteSize::new(0).is_none());
    assert!(MilliCpus::new(0).is_none());
    assert!(MilliCpus::new(u32::MAX).is_none());
}
