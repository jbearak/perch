// SPDX-License-Identifier: GPL-3.0-only

//! Builds `perch-vt`, Perch's static library over Ghostty's `ghostty-vt`
//! module, the way Ghostty builds its own static library: position-independent
//! code with `compiler_rt` and the UBSan runtime bundled. `crates/perch/build.rs`
//! runs this with the shipped flags; the defaults below are the same, so a
//! plain `zig build test` tests the shipped configuration.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const simd = b.option(
        bool,
        "simd",
        "Ghostty's SIMD code paths (pull in C++ and libc; off for Perch)",
    ) orelse false;
    const vt_features = b.option(
        []const u8,
        "vt-features",
        "libghostty-vt feature modifications, passed through to Ghostty",
    ) orelse "-kitty-graphics";
    const test_observer = b.option(
        bool,
        "test-observer",
        "Record parsed queries for Perch's differential tests (test-hooks builds only)",
    ) orelse false;

    const ghostty = b.dependency("ghostty", .{
        .target = target,
        .optimize = optimize,
        .simd = simd,
        .@"vt-features" = vt_features,
    });

    const options = b.addOptions();
    options.addOption(bool, "test_observer", test_observer);

    const mod = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .target = target,
        .optimize = optimize,
        // Rust links position-independent executables on Linux.
        .pic = true,
    });
    mod.addImport("ghostty-vt", ghostty.module("ghostty-vt"));
    mod.addOptions("perch_vt_options", options);

    const lib = b.addLibrary(.{
        .name = "perch-vt",
        .linkage = .static,
        .root_module = mod,
    });
    // A static library must carry its own runtime, or the final link has
    // undefined symbols.
    lib.bundle_compiler_rt = true;
    lib.bundle_ubsan_rt = true;
    // As in Ghostty: the self-hosted x86_64 backend does not work for Darwin.
    if (target.result.os.tag.isDarwin()) lib.use_llvm = true;
    b.installArtifact(lib);

    const tests = b.addTest(.{ .root_module = mod });
    const run_tests = b.addRunArtifact(tests);
    const test_step = b.step("test", "Run perch-vt tests");
    test_step.dependOn(&run_tests.step);
}
