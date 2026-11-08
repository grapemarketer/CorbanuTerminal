use super::*;

/// #1863: a `\\localhost` UNC cwd is an error, not a panic in `url`.
#[test]
fn guardian_cwd_refuses_localhost_unc_path() {
    let native_cwd = AbsolutePathBuf::from_absolute_path_checked(r"\\localhost\C$\workspace")
        .expect("UNC path should be absolute");
    let cwd = PathUri::from_abs_path(&native_cwd);

    let err = guardian_cwd(codex_exec_server::LOCAL_ENVIRONMENT_ID, cwd)
        .expect_err("a localhost UNC cwd should be refused");
    assert!(err.to_string().contains("network (UNC) path"), "{err}");
}
