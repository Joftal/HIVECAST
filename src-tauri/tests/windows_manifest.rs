//! Windows 测试可执行文件需要 Common Controls v6 的 manifest 才能启动。
//!
//! Cargo 只有在包里存在测试目标时才会接受 build.rs 的
//! `cargo:rustc-link-arg-tests`；这个集成测试目标就是那个锚点。
//! 真正的断言在 lib 单测里，这里只确认测试二进制能正常链接并运行。

#[test]
fn test_binary_starts() {
    assert_eq!(2 + 2, 4);
}
