use functions::rust_coreutils_static_assert;

#[link(name = "test")]
extern "C" {
    fn coreutils_static_assert();
}

#[cfg_attr(test, test)]
pub fn test_coreutils_static_assert() {
    unsafe {
        coreutils_static_assert();
        rust_coreutils_static_assert();
    }
}
