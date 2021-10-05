use std::{
  alloc::{Layout, LayoutError},
  marker::PhantomPinned,
  ops::{Deref, DerefMut},
  sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering},
};

#[cfg(target_family = "unix")]
use libc::c_void;
#[cfg(target_family = "windows")]
use winapi::{
  ctypes::c_void,
  um::{memoryapi, sysinfoapi, winnt},
};

mod err;
pub use err::Error;

struct HolderInner<T> {
  value: T,
  _marker: PhantomPinned,
}

pub struct SensitiveData<T> {
  memory_layout: Layout,
  inner_ptr: *mut HolderInner<T>,
  deref_counter: AtomicUsize,
}

pub struct DerefHolder<'holder, T> {
  holder: &'holder SensitiveData<T>,
  changed_permissions: AtomicBool,
}

pub struct DerefMutHolder<'holder, T> {
  holder: &'holder mut SensitiveData<T>,
}

impl<T> Drop for DerefMutHolder<'_, T> {
  fn drop(&mut self) {
    self.holder
        .make_inaccessible()
        .expect("Could not make SensitiveData inaccessible");
  }
}

impl<T> Drop for DerefHolder<'_, T> {
  fn drop(&mut self) {
    if self.changed_permissions.load(Ordering::Acquire) {
      if self.holder.deref_counter.fetch_sub(1, Ordering::AcqRel) == 1 {
        self.holder
            .make_inaccessible()
            .expect("Could not make SensitiveData readable");
      }
    }
  }
}

impl<T> Drop for SensitiveData<T> {
  fn drop(&mut self) {
    self.make_writable()
        .expect("Could not make SensitiveData writable");
    unsafe {
      std::ptr::drop_in_place(self.inner_ptr);
    }
    self.zeroize_inner();
    unsafe {
      std::alloc::dealloc(self.inner_ptr as *mut u8, self.memory_layout);
    }
  }
}

impl<'deref_holder, T> Deref for DerefHolder<'_, T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    if !self.changed_permissions.swap(true, Ordering::AcqRel) {
      if self.holder.deref_counter.fetch_add(1, Ordering::AcqRel) == 0 {
        self.holder
            .make_readable()
            .expect("Could not make SensitiveData readable");
      }
    }
    unsafe { &(*self.holder.inner_ptr).value }
  }
}

impl<T> Deref for DerefMutHolder<'_, T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    self.holder
        .make_readable()
        .expect("Could not make SensitiveData readable");
    unsafe { &(*self.holder.inner_ptr).value }
  }
}

impl<T> DerefMut for DerefMutHolder<'_, T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    self.holder
        .make_writable()
        .expect("Could not make SensitiveData writable");
    unsafe { &mut (*self.holder.inner_ptr).value }
  }
}

#[cfg(target_family = "unix")]
#[inline(always)]
fn page_size() -> usize {
  (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }) as usize
}

#[cfg(target_family = "windows")]
#[inline(always)]
fn page_size() -> usize {
  let mut system_info = sysinfoapi::SYSTEM_INFO::default();
  unsafe { sysinfoapi::GetSystemInfo(&mut system_info as *mut _) };
  system_info.dwPageSize as usize
}

impl<T: Sized> SensitiveData<T> {
  fn layout() -> Result<Layout, LayoutError> {
    Ok(Layout::new::<T>().align_to(page_size())?.pad_to_align())
  }

  #[cfg(target_family = "unix")]
  #[inline(always)]
  fn lock_memory(&mut self) -> Result<(), std::io::Error> {
    if unsafe { libc::mlock(self.inner_ptr as *mut c_void, self.memory_layout.size()) } == 0 {
      Ok(())
    } else {
      Err(std::io::Error::last_os_error())
    }
  }

  #[cfg(target_family = "windows")]
  #[inline(always)]
  fn lock_memory(&mut self) -> Result<(), std::io::Error> {
    if unsafe { memoryapi::VirtualLock(self.inner_ptr as *mut c_void, self.memory_layout.size()) }
       != 0
    {
      Ok(())
    } else {
      Err(std::io::Error::last_os_error())
    }
  }

  fn new_holder() -> Result<Self, Error> {
    use std::alloc::alloc;
    let memory_layout = Self::layout()?;
    let inner_ptr;
    unsafe {
      let allocated = alloc(memory_layout);
      inner_ptr = allocated as *mut HolderInner<T>;
    }
    let mut data = SensitiveData { memory_layout,
                                   inner_ptr,
                                   deref_counter: AtomicUsize::new(0) };
    data.lock_memory()?;
    Ok(data)
  }

  pub unsafe fn new_zeroed() -> Result<Self, Error> {
    let mut holder = Self::new_holder()?;
    holder.zeroize_inner();
    holder.make_inaccessible()
          .expect("Could not make the new SensitiveData inaccessible");
    Ok(holder)
  }

  pub fn new(t: T) -> Result<Self, Error> {
    let holder = Self::new_holder()?;
    unsafe {
      std::ptr::write(holder.inner_ptr,
                      HolderInner { value: t,
                                    _marker: PhantomPinned })
    }
    holder.make_inaccessible()
          .expect("Could not make the new SensitiveData inaccessible");
    Ok(holder)
  }

  #[inline(always)]
  fn zeroize_inner(&mut self) {
    use std::{mem::zeroed, ptr::write_volatile};
    unsafe { write_volatile(self.inner_ptr, zeroed()) }
    fence(Ordering::Release);
  }

  #[cfg(target_family = "unix")]
  fn make_inaccessible(&self) -> Result<(), err::IoError> {
    if unsafe {
      libc::mprotect(self.inner_ptr as *mut c_void,
                     self.memory_layout.size(),
                     libc::PROT_NONE)
    } == 0
    {
      Ok(())
    } else {
      Err(err::IoError::last_os_error())
    }
  }

  #[cfg(target_family = "windows")]
  fn make_inaccessible(&self) -> Result<(), err::IoError> {
    use std::ptr::addr_of_mut;
    if unsafe {
      let mut _old_protect = 0;
      memoryapi::VirtualProtect(self.inner_ptr as *mut c_void,
                                self.memory_layout.size(),
                                winnt::PAGE_NOACCESS,
                                addr_of_mut!(_old_protect))
    } != 0
    {
      Ok(())
    } else {
      Err(err::IoError::last_os_error())
    }
  }

  #[cfg(target_family = "unix")]
  fn make_readable(&self) -> Result<(), err::IoError> {
    if unsafe {
      libc::mprotect(self.inner_ptr as *mut c_void,
                     self.memory_layout.size(),
                     libc::PROT_READ)
    } == 0
    {
      Ok(())
    } else {
      Err(err::IoError::last_os_error())
    }
  }

  #[cfg(target_family = "windows")]
  fn make_readable(&self) -> Result<(), err::IoError> {
    use std::ptr::addr_of_mut;
    if unsafe {
      let mut _old_protect = 0;
      memoryapi::VirtualProtect(self.inner_ptr as *mut c_void,
                                self.memory_layout.size(),
                                winnt::PAGE_READONLY,
                                addr_of_mut!(_old_protect))
    } != 0
    {
      Ok(())
    } else {
      Err(err::IoError::last_os_error())
    }
  }

  #[cfg(target_family = "unix")]
  fn make_writable(&mut self) -> Result<(), err::IoError> {
    if unsafe {
      libc::mprotect(self.inner_ptr as *mut c_void,
                     self.memory_layout.size(),
                     libc::PROT_READ | libc::PROT_WRITE)
    } == 0
    {
      Ok(())
    } else {
      Err(err::IoError::last_os_error())
    }
  }

  #[cfg(target_family = "windows")]
  fn make_writable(&self) -> Result<(), err::IoError> {
    use std::ptr::addr_of_mut;
    if unsafe {
      let mut _old_protect = 0;
      memoryapi::VirtualProtect(self.inner_ptr as *mut c_void,
                                self.memory_layout.size(),
                                winnt::PAGE_READWRITE,
                                addr_of_mut!(_old_protect))
    } != 0
    {
      Ok(())
    } else {
      Err(err::IoError::last_os_error())
    }
  }

  #[inline(always)]
  pub fn borrow(&self) -> DerefHolder<T> {
    DerefHolder { holder: self,
                  changed_permissions: AtomicBool::new(false) }
  }

  #[inline(always)]
  pub fn borrow_mut(&mut self) -> DerefMutHolder<T> {
    DerefMutHolder { holder: self }
  }

  #[inline(always)]
  pub fn assert_no_borrows(&mut self) {}

  #[inline(always)]
  pub fn assert_no_mut_borrows(&self) {}
}

#[cfg(test)]
mod tests {
  use super::*;
  struct SomeTestStruct {
    a: u8,
  }

  struct WithDestructor {
    destructor_executed: *mut bool,
  }

  impl Drop for WithDestructor {
    fn drop(&mut self) {
      println!("Saved pointer {:p}", self.destructor_executed);
      unsafe {
        *self.destructor_executed = true;
      }
    }
  }

  #[test]
  fn zeroized_when_created() {
    let a: SensitiveData<SomeTestStruct> = unsafe { SensitiveData::new_zeroed().unwrap() };
    assert_eq!(a.borrow().a, 0);
  }

  #[test]
  fn pads_to_page() {
    let a: SensitiveData<SomeTestStruct> = SensitiveData::new(SomeTestStruct { a: 0 }).unwrap();
    assert_eq!(a.memory_layout.size(), a.memory_layout.align());
  }

  #[test]
  fn value_when_created() {
    let a: SensitiveData<SomeTestStruct> = SensitiveData::new(SomeTestStruct { a: 5 }).unwrap();
    assert_eq!(a.borrow().a, 5);
  }

  #[test]
  fn destructor_executed() {
    let mut a: SensitiveData<WithDestructor> = unsafe { SensitiveData::new_zeroed().unwrap() };
    let mut destructor_executed = false;
    let ptr: &mut bool = &mut destructor_executed;
    println!("Real pointer {:p}", ptr);
    {
      a.borrow_mut().destructor_executed = ptr as *mut bool;
      println!("Borrowed pointer {:p}", a.borrow().destructor_executed);
    }
    assert_eq!(*ptr, false);
    drop(a);
    assert_eq!(*ptr, true);
  }
  #[test]
  fn multiple_readers() {
    let a: SensitiveData<SomeTestStruct> = unsafe { SensitiveData::new_zeroed().unwrap() };
    let _b = a.borrow();
    let _c = a.borrow();
  }
  #[test]
  fn reader_then_writer_then_reader() {
    let mut a: SensitiveData<SomeTestStruct> = unsafe { SensitiveData::new_zeroed().unwrap() };
    {
      let _b = a.borrow();
    }
    {
      let _c = a.borrow_mut();
    }
    let _c = a.borrow();
  }
}
