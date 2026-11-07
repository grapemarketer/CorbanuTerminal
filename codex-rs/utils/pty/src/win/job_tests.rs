use super::JobObject;
use pretty_assertions::assert_eq;
use std::os::windows::io::AsRawHandle;
use winapi::um::jobapi2::QueryInformationJobObject;
use winapi::um::winnt::JOB_OBJECT_LIMIT_BREAKAWAY_OK;
use winapi::um::winnt::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
use winapi::um::winnt::JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
use winapi::um::winnt::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
use winapi::um::winnt::JobObjectExtendedLimitInformation;

const FLAGS: u32 = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    | JOB_OBJECT_LIMIT_BREAKAWAY_OK
    | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;

fn limit_flags(job: &JobObject) -> u32 {
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `job` holds a valid job handle; the buffer and its size match.
    let ok = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle().cast(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of_mut!(limits).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
    limits.BasicLimitInformation.LimitFlags & FLAGS
}

#[test]
fn default_job_allows_breakaway_before_and_after_preserving() {
    let job = JobObject::create().expect("create job");
    assert_eq!(
        limit_flags(&job),
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK
    );
    job.preserve_descendants().expect("preserve descendants");
    assert_eq!(limit_flags(&job), JOB_OBJECT_LIMIT_BREAKAWAY_OK);
}

/// #733: a job without breakaway keeps it off when its descendants are
/// preserved, so they can't leave it later.
#[test]
fn job_without_breakaway_never_allows_it() {
    let job = JobObject::create_without_breakaway().expect("create job");
    assert_eq!(limit_flags(&job), JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
    job.preserve_descendants().expect("preserve descendants");
    assert_eq!(limit_flags(&job), 0);
}
