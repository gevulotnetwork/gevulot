use std::ffi::{c_char, CStr, CString};

#[repr(C)]
pub struct Task {
    id: *const c_char,
    pub args: *const *const c_char,
    pub files: *const *const c_char,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "id: {:#?}", self.id)?;
        write!(f, "args: {:#?}", self.args)?;
        write!(f, "files: {:#?}", self.files)
    }
}

#[repr(C)]
pub struct TaskResult {
    data: Vec<u8>,
    files: Vec<String>,
}

#[no_mangle]
pub extern "C" fn new_task_result(data: *mut u8, len: usize) -> *mut TaskResult {
    // SAFETY: 1. This is safe as long as passed `data` and `len` arguments are valid.
    unsafe {
        let data = if data.is_null() {
            vec![]
        } else {
            let slice = std::slice::from_raw_parts(data, len);
            slice.to_vec()
        };

        Box::into_raw(Box::new(TaskResult {
            data,
            files: vec![],
        }))
    }
}

#[no_mangle]
pub extern "C" fn add_file_to_result(result: *mut TaskResult, file_name: *const c_char) {
    // SAFETY: Following is only safe if the passed `file_name` is:
    // 1. valid pointer
    // 2. null terminated.
    let c_str = unsafe { CStr::from_ptr(file_name) };

    // SAFETY: `result` pointer dereferencing is safe because that pointer
    // originates from this library (above).
    unsafe {
        (*result)
            .files
            .push(c_str.to_string_lossy().into_owned().clone());
    }
}

#[no_mangle]
pub extern "C" fn run(callback: extern "C" fn(*const Task) -> *mut TaskResult) {
    let res = gevulot_shim::run(|task: &gevulot_shim::Task| -> Result<gevulot_shim::TaskResult, Box<dyn std::error::Error>> {
        let id_cstr = CString::new(task.id.clone()).expect("task.id");

        // Convert Vec<String> -> *const *const c_char.
        let cstr_args: Vec<CString> = task.args.iter().map(|s| CString::new(s.as_str()).expect("task.arg")).collect();
        let cstr_files: Vec<CString> = task.files.iter().map(|s| CString::new(s.as_str()).expect("task.file")).collect();

        let mut carr_args: Vec<*const c_char> = cstr_args.iter().map(|s| s.as_ptr()).collect();
        let mut carr_files: Vec<*const c_char> = cstr_files.iter().map(|s| s.as_ptr()).collect();

        // Terminate the arrays.
        carr_args.push(std::ptr::null());
        carr_files.push(std::ptr::null());

        let c_task = Task{
            id: id_cstr.as_ptr(),
            args: carr_args.as_ptr(),
            files: carr_files.as_ptr(),
        };

        let result = callback(&c_task);

        let data;
        let files;

        // SAFETY: Given that pointer of `result` is valid, the internals of 
        // TaskResult are handled within this library and are therefore
        // expected to be safe.
        unsafe {
            files = (*result).files.clone();
            data = (*result).data.clone();
        }

        task.result(data, files)
    });

    if res.is_err() {
        eprintln!("error: {:#?}", res.unwrap())
    }
}
