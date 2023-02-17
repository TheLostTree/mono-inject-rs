use _core::mem::MaybeUninit;
use _core::str::FromStr;
use std::ffi::CStr;
use std::ffi::CString;
use std::io;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Read;
use std::mem;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;
use widestring::WideCString;
use winapi::um::handleapi::CloseHandle;

use _core::ffi::c_void;
use winapi::shared::minwindef::BOOL;
use winapi::shared::minwindef::DWORD;
use winapi::shared::minwindef::FALSE;
use winapi::shared::minwindef::HMODULE;
use winapi::shared::minwindef::MAX_PATH;
use winapi::shared::ntdef::LPWSTR;
use winapi::shared::ntdef::NULL;
use winapi::shared::ntdef::TRUE;
use winapi::um::dbghelp::SymEnumSymbolsW;
use winapi::um::fileapi::ReadFile;
use winapi::um::handleapi as whandle;
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::libloaderapi as wload;
use winapi::um::libloaderapi::GetModuleHandleA;
use winapi::um::libloaderapi::GetProcAddress;
use winapi::um::libloaderapi::LoadLibraryExA;
use winapi::um::libloaderapi::LoadLibraryExW;
use winapi::um::libloaderapi::DONT_RESOLVE_DLL_REFERENCES;
use winapi::um::memoryapi as wmem;
use winapi::um::minwinbase::LPTHREAD_START_ROUTINE;
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
use winapi::um::namedpipeapi::ConnectNamedPipe;
use winapi::um::processthreadsapi as wproc;
use winapi::um::processthreadsapi::CreateRemoteThread;
use winapi::um::processthreadsapi::OpenProcess;
use winapi::um::psapi::EnumProcessModules;
use winapi::um::psapi::GetModuleFileNameExW;
use winapi::um::securitybaseapi::InitializeSecurityDescriptor;
use winapi::um::securitybaseapi::SetSecurityDescriptorDacl;
use winapi::um::tlhelp32::{
    CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
};

use std::slice;
use winapi::um::winbase::CreateNamedPipeA;
use winapi::um::winbase::LocalAlloc;
use winapi::um::winbase::FILE_FLAG_FIRST_PIPE_INSTANCE;
use winapi::um::winbase::PIPE_ACCESS_DUPLEX;
use winapi::um::winbase::PIPE_READMODE_BYTE;
use winapi::um::winbase::PIPE_TYPE_BYTE;
use winapi::um::winbase::PIPE_WAIT;
use winapi::um::winnt::HANDLE;
use winapi::um::winnt::IMAGE_DIRECTORY_ENTRY_EXPORT;
use winapi::um::winnt::IMAGE_DOS_HEADER;
use winapi::um::winnt::IMAGE_EXPORT_DIRECTORY;
use winapi::um::winnt::IMAGE_NT_HEADERS;
use winapi::um::winnt::IMAGE_NT_HEADERS64;
use winapi::um::winnt::PIMAGE_DOS_HEADER;
use winapi::um::winnt::PIMAGE_EXPORT_DIRECTORY;
use winapi::um::winnt::PIMAGE_NT_HEADERS;
use winapi::um::winnt::PROCESS_ALL_ACCESS;
use winapi::um::winnt::PSECURITY_DESCRIPTOR;
use winapi::um::winnt::SECURITY_DESCRIPTOR_MIN_LENGTH;
use winapi::um::winnt::SECURITY_DESCRIPTOR_REVISION;
use winapi::um::winnt::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE};
use winapi::*;

use argparse::{ArgumentParser, Store, StoreTrue};
use simple_logger::SimpleLogger;
mod defs;
mod proc;
mod utils;

#[macro_export]
macro_rules! werr {
    ($cond:expr) => {
        if $cond {
            let e = std::io::Error::last_os_error();
            println!("windows error: {:?}", e);
            return Err(e);
        }
    };
}

fn inject(proc: HANDLE, dll: &Path) -> io::Result<()> {
    let full_path = dll.canonicalize()?;
    let full_path = full_path.as_os_str();
    let full_path = WideCString::from_os_str(full_path).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("invalid dll path: {:?}", e),
        )
    })?;

    let path_len = (full_path.len() * 2) + 1;
    // allocate space for the path inside target proc
    let dll_addr = unsafe {
        wmem::VirtualAllocEx(
            proc,
            ptr::null_mut(),
            path_len,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_EXECUTE_READWRITE,
        )
    };

    werr!(dll_addr.is_null());
    println!("allocated remote memory @ {:?}", dll_addr);

    let res = unsafe {
        // write dll inside target process
        wmem::WriteProcessMemory(
            proc,
            dll_addr,
            full_path.as_ptr() as *mut _,
            path_len,
            ptr::null_mut(),
        )
    };

    werr!(res == 0);

    let krnl = CString::new("kernel32.dll").unwrap();
    let krnl = unsafe { wload::GetModuleHandleA(krnl.as_ptr()) };

    let loadlib = CString::new("LoadLibraryW").unwrap();
    let loadlib = unsafe { wload::GetProcAddress(krnl, loadlib.as_ptr()) };
    println!("found LoadLibraryW for injection at {:?}", loadlib);

    let hthread = unsafe {
        wproc::CreateRemoteThread(
            proc,
            ptr::null_mut(),
            0,
            Some(mem::transmute(loadlib)),
            dll_addr,
            0,
            ptr::null_mut(),
        )
    };

    werr!(hthread.is_null());
    println!("spawned remote thread at {:?}", hthread);
    unsafe {
        whandle::CloseHandle(hthread);
    }

    Ok(())
}

fn get_mono_loader() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap();

    let loc = dir.join("mono_lib.dll").to_path_buf();
    println!("location: {}", loc.display());
    Ok(loc)
}

fn main() -> io::Result<()> {
    let mut target = String::new();
    let mut dll_inject = String::new();
    let mut namespace_arg = String::new();
    let mut class_arg = String::new();
    let mut method_arg = String::new();
    let mut mono_module_arg = String::from("mono.dll");
    {
        let mut ap = ArgumentParser::new();
        ap.refer(&mut mono_module_arg)
            .add_option(&["--module"], Store, "mono.dll is default");
        ap.refer(&mut target)
            .add_option(&["--process"], Store, "target process e.g ravenfield.exe")
            .required();
        ap.refer(&mut dll_inject)
            .add_option(&["--dll"], Store, "path to the mono dll hack")
            .required();
        ap.refer(&mut namespace_arg)
            .add_option(&["--namespace"], Store, "namespace")
            .required();
        ap.refer(&mut class_arg)
            .add_option(&["--class"], Store, "class name")
            .required();

        ap.refer(&mut method_arg)
            .add_option(&["--method"], Store, "method name")
            .required();

        ap.parse_args_or_exit();
    }

    SimpleLogger::new().init().unwrap();
    //log::warn!("This is an example message.");
    //let pid = proc::get_pid(&String::from("ravenfield.exe"));
    let pid = proc::get_pid(&target);

    if pid == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Process not found - pid empty",
        ));
    }

    let h_proc = unsafe { OpenProcess(PROCESS_ALL_ACCESS, 0, pid) };

    log::debug!("hproc value {:p}", h_proc);

    let mono_load_path = get_mono_loader().unwrap();
    inject(
        h_proc,
        &mono_load_path, // important  derDLL.dll"
    )
    .unwrap();

    let pipe_name = format!("{}{}", "\\\\.\\pipe\\MLPIPE_", pid);

    let mono_load_path_str = String::from_str(mono_load_path.to_str().unwrap()).unwrap();
    let mono_module = String::from("mono_lib.dll"); // ".dll"  .// HERE LATETS GET FROM MONO LIB  | ALSO ADD ERRORS CHECK
    let mono_inject_func = String::from("inject"); // Inject

    let named_pipe = CString::new(pipe_name.clone()).unwrap();

    /////////////// https://stackoverflow.com/questions/29278089/how-to-update-libcc-char-array-with-string
    //let mut arr1: ArrType = [0; 250];

    let arr1 = utils::str_arr(&dll_inject);
    let arr2 = utils::str_arr(&namespace_arg);
    let arr3 = utils::str_arr(&class_arg);
    let arr4 = utils::str_arr(&method_arg);
    let arr5 = utils::str_arr(&pipe_name.as_str());

    //let x2: Vec<char> = String::from("RavenfieldHax").chars().collect();
    let mut loader_args = defs::LoaderArguments {
        dll_path: arr1,
        loader_namespace: arr2,
        loader_classname: arr3,
        loader_methodname: arr4,
        loader_pipename: arr5,
    };

    println!("{:?}", loader_args.dll_path);
    println!("{:?}", loader_args.loader_namespace);
    println!("{:?}", loader_args.loader_classname);
    println!("{:?}", loader_args.loader_methodname);
    println!("{:?}", loader_args.loader_pipename);

    let address_params = unsafe {
        wmem::VirtualAllocEx(
            h_proc,
            ptr::null_mut(),
            std::mem::size_of::<defs::LoaderArguments>(),
            MEM_RESERVE | MEM_COMMIT,
            PAGE_EXECUTE_READWRITE,
        )
    };

    werr!(address_params.is_null());
    println!(
        "(address_params) allocated remote memory @ {:?}",
        address_params
    );

    let loader_args_ptr: *mut c_void = &mut loader_args as *mut _ as *mut c_void;

    let res = unsafe {
        // write dll inside target process
        wmem::WriteProcessMemory(
            h_proc,
            address_params,
            loader_args_ptr.cast(),
            std::mem::size_of::<defs::LoaderArguments>(),
            ptr::null_mut(),
        )
    };

    println!("WriteProcess memory loader params: {:?}", res);
    werr!(res == 0);
    println!("Parameter struct written to target..");

    let func_offset_loader = proc::mono_loader_func(mono_load_path_str, mono_inject_func).unwrap();
    println!("Funcoffset of Loader?: {:?}", func_offset_loader);
    let injected_loader_base = proc::module_handles(h_proc, &mono_module);
    println!("injectedLoader module remote {}", injected_loader_base);
    // mono_loader_func_address_final
    let target_fun_addr = injected_loader_base + func_offset_loader;

    println!("Lpthreadstart is {:x}", target_fun_addr);
    unsafe {
        CreateRemoteThread(
            h_proc,
            std::ptr::null_mut(),
            0,
            std::mem::transmute::<_, LPTHREAD_START_ROUTINE>(target_fun_addr),
            address_params,
            0,
            std::ptr::null_mut(),
        );
    }

    println!("Injected  example");

    let p_security_desc = unsafe { LocalAlloc(0, SECURITY_DESCRIPTOR_MIN_LENGTH) };

    unsafe { InitializeSecurityDescriptor(p_security_desc, SECURITY_DESCRIPTOR_REVISION) };

    unsafe { SetSecurityDescriptorDacl(p_security_desc, 1, std::ptr::null_mut(), FALSE) };

    println!("pSecurityDesc - {:p}", p_security_desc);

    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
        lpSecurityDescriptor: p_security_desc,
        bInheritHandle: FALSE,
    };

    let h_pipe = unsafe {
        CreateNamedPipeA(
            named_pipe.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            4096,
            4096,
            1,
            &security_attributes as *const _ as *mut _,
        )
    };

    werr!(h_pipe.is_null());
    println!("h_pipe value: {:?}", h_pipe);
    let ress = unsafe { ConnectNamedPipe(h_pipe, std::ptr::null_mut()) };

    if ress != 0 {
        println!("connected to named pipe");
    }
    println!("h_pipe suc");
    //let mut buf = vec![0u8; PIPE_BUFFER_SIZE.try_into().unwrap()];

    let mut exit_loop = 0;
    while exit_loop == 0 {
        //let mut buf: Vec<char> = Vec::new();
        let mut buffer: [u8; 1024] = [0; 1024];
        let mut bytes_read: u32 = 0;

        let res_read = unsafe {
            ReadFile(
                h_pipe,
                buffer.as_mut_ptr() as *mut _,
                1024, //buf.len().try_into().unwrap(),
                &mut bytes_read as *mut _,
                std::ptr::null_mut(),
            )
        };

        if res_read != 0 {
            println!("-Received result from mono load lib");

            let message = String::from_utf8_lossy(&buffer[0..bytes_read as usize]).to_string();
            println!("{}", message);

            // let msg: String = buf.into_iter().collect();
            // println!("message {}", msg);
            exit_loop = 1;
        }
    }

    // let bytes_read: usize = bytes_read.try_into().unwrap_or(0);
    //werr!(res_read == 0);

    Ok(())
}
