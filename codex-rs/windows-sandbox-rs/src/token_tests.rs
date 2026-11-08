use super::*;
use windows_sys::Win32::Security::EqualSid;
use windows_sys::Win32::Security::TokenRestrictedSids;

unsafe fn token_has_restricting_sid(token: HANDLE, expected_sid: *mut c_void) -> Result<bool> {
    let mut needed = 0;
    GetTokenInformation(
        token,
        TokenRestrictedSids,
        std::ptr::null_mut(),
        0,
        &mut needed,
    );
    if needed == 0 {
        return Err(anyhow!(
            "GetTokenInformation(TokenRestrictedSids) size query failed: {}",
            GetLastError()
        ));
    }

    let mut buffer = vec![0_u8; needed as usize];
    if GetTokenInformation(
        token,
        TokenRestrictedSids,
        buffer.as_mut_ptr().cast(),
        needed,
        &mut needed,
    ) == 0
    {
        return Err(anyhow!(
            "GetTokenInformation(TokenRestrictedSids) failed: {}",
            GetLastError()
        ));
    }

    let group_count = std::ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) as usize;
    let after_count = buffer.as_ptr().add(std::mem::size_of::<u32>()) as usize;
    let align = std::mem::align_of::<SID_AND_ATTRIBUTES>();
    let entries_addr = (after_count + (align - 1)) & !(align - 1);
    let restricting_sids =
        std::slice::from_raw_parts(entries_addr as *const SID_AND_ATTRIBUTES, group_count);
    Ok(restricting_sids
        .iter()
        .any(|entry| EqualSid(entry.Sid, expected_sid) != 0))
}

/// BUILTIN\Users, which the test process holds; it stands in for the sandbox
/// users' group, which it doesn't (#1795).
const TEST_GROUP_SID: &str = "S-1-5-32-545";

#[test]
fn elevated_token_includes_network_proxy_restricting_sid() -> Result<()> {
    let capability_sid = LocalSid::from_string("S-1-5-21-10-20-30-40")?;
    let network_proxy_sid = LocalSid::from_string("S-1-5-21-50-60-70-80")?;
    let group_sid = LocalSid::from_string(TEST_GROUP_SID)?;
    let base_token = unsafe { get_current_token_for_restriction()? };
    let restricted_token = unsafe {
        create_elevated_token_with_group(
            base_token,
            &[capability_sid.as_ptr()],
            &[network_proxy_sid.as_ptr()],
            group_sid.as_ptr(),
        )?
    };

    let has_network_proxy_sid =
        unsafe { token_has_restricting_sid(restricted_token, network_proxy_sid.as_ptr()) };
    unsafe {
        CloseHandle(restricted_token);
        CloseHandle(base_token);
    }

    assert!(has_network_proxy_sid?);
    Ok(())
}

/// D324: the public elevated token builders refuse a base token outside the
/// sandbox users' group, as the test process is.
#[test]
fn elevated_token_builders_refuse_a_caller_outside_the_sandbox_users_group() -> Result<()> {
    let capability_sid = LocalSid::from_string("S-1-5-21-10-20-30-40")?;
    let network_proxy_sid = LocalSid::from_string("S-1-5-21-50-60-70-80")?;
    let base_token = unsafe { get_current_token_for_restriction()? };
    let results = unsafe {
        [
            create_readonly_token_with_caps_and_user_from(
                base_token,
                &[capability_sid.as_ptr()],
                &[network_proxy_sid.as_ptr()],
            ),
            create_workspace_write_token_with_caps_and_user_from(
                base_token,
                &[capability_sid.as_ptr()],
                &[network_proxy_sid.as_ptr()],
            ),
        ]
    };
    let mut refusals = Vec::new();
    for result in results {
        match result {
            Ok(token) => {
                unsafe { CloseHandle(token) };
                refusals.push(None);
            }
            Err(err) => refusals.push(Some(format!("{err:#}"))),
        }
    }
    unsafe { CloseHandle(base_token) };

    for refusal in refusals {
        let refusal = refusal.expect("a token for a caller outside CodexSandboxUsers");
        assert!(refusal.contains(SANDBOX_USERS_GROUP), "{refusal}");
    }
    Ok(())
}
