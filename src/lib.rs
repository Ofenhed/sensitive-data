use std::{
  alloc::Layout,
  marker::PhantomPinned,
  ops::{Deref, DerefMut},
  sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering},
};

struct HolderInner<T> {
  value: T,
  _marker: PhantomPinned,
}

pub struct SensitiveData<T> {
  inner_size: usize,
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
    self.holder.make_inaccessible();
  }
}

impl<T> Drop for DerefHolder<'_, T> {
  fn drop(&mut self) {
    if self.changed_permissions.load(Ordering::Acquire) {
      if self.holder.deref_counter.fetch_sub(1, Ordering::AcqRel) == 1 {
        self.holder.make_inaccessible();
      }
    }
  }
}

impl<T> Drop for SensitiveData<T> {
  fn drop(&mut self) {
    self.make_writable();
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
        self.holder.make_readable();
      }
    }
    unsafe { &(*self.holder.inner_ptr).value }
  }
}

impl<T> Deref for DerefMutHolder<'_, T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    self.holder.make_readable();
    unsafe { &(*self.holder.inner_ptr).value }
  }
}

impl<T> DerefMut for DerefMutHolder<'_, T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    self.holder.make_writable();
    unsafe { &mut (*self.holder.inner_ptr).value }
  }
}

impl<T: Sized> SensitiveData<T> {
  #[cfg(target_family = "unix")]
  #[inline(always)]
  fn new_holder() -> Self {
    use std::{alloc::alloc, mem::size_of};
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let object_size = size_of::<HolderInner<T>>();
    let block_size = object_size / page_size;
    let block_size = if block_size < object_size {
      block_size + page_size
    } else {
      block_size
    };
    let memory_layout =
      Layout::from_size_align(block_size, page_size).unwrap_or_else(|_| {
                                                      panic!("Invalid pagesize: {}", page_size)
                                                    });
    let inner_ptr;
    unsafe {
      let allocated = alloc(memory_layout);
      inner_ptr = allocated as *mut HolderInner<T>;
      libc::mlock(inner_ptr as *mut libc::c_void, block_size);
    }
    SensitiveData { inner_size: object_size,
                    memory_layout,
                    inner_ptr,
                    deref_counter: AtomicUsize::new(0) }
  }

  #[inline(always)]
  pub unsafe fn new_zeroed() -> Self {
    let mut holder = Self::new_holder();
    holder.zeroize_inner();
    holder.make_inaccessible();
    holder
  }

  #[inline(always)]
  pub fn new(t: T) -> Self {
    let holder = Self::new_holder();
    unsafe {
      std::ptr::write(holder.inner_ptr,
                      HolderInner { value: t,
                                    _marker: PhantomPinned })
    }
    holder.make_inaccessible();
    holder
  }

  #[inline(always)]
  fn zeroize<const BYTES_ZEROIZED: usize>(&mut self, offset: isize) {
    use std::ptr::write_volatile;

    unsafe {
      write_volatile((self.inner_ptr as *mut [u8; BYTES_ZEROIZED]).offset(offset),
                     [0u8; BYTES_ZEROIZED])
    }
    fence(Ordering::Release);
  }

  fn zeroize_inner(&mut self) {
    let mut offset = 0;
    let inner_size = self.inner_size as isize;
    while offset < inner_size {
      if inner_size - offset >= 4096 {
        self.zeroize::<4096>(offset);
        offset += 4096;
      } else if inner_size - offset >= 512 {
        self.zeroize::<512>(offset);
        offset += 512;
      } else if inner_size - offset >= 8 {
        self.zeroize::<8>(offset);
        offset += 8;
      } else {
        self.zeroize::<1>(offset);
        offset += 1;
      }
    }
  }

  #[cfg(target_family = "unix")]
  fn make_inaccessible(&self) {
    if unsafe {
      libc::mprotect(self.inner_ptr as *mut libc::c_void,
                     self.memory_layout.size(),
                     libc::PROT_NONE)
    } != 0
    {
      panic!("Could not make memory inaccessible");
    }
  }

  #[cfg(target_family = "unix")]
  fn make_readable(&self) {
    if unsafe {
      libc::mprotect(self.inner_ptr as *mut libc::c_void,
                     self.memory_layout.size(),
                     libc::PROT_READ)
    } != 0
    {
      panic!("Could not make memory read only");
    }
  }

  #[cfg(target_family = "unix")]
  fn make_writable(&mut self) {
    if unsafe {
      libc::mprotect(self.inner_ptr as *mut libc::c_void,
                     self.memory_layout.size(),
                     libc::PROT_READ | libc::PROT_WRITE)
    } != 0
    {
      panic!("Could not make memory inaccessible");
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
    let a: SensitiveData<SomeTestStruct> = unsafe { SensitiveData::new_zeroed() };
    assert_eq!(a.borrow().a, 0);
  }

  #[test]
  fn value_when_created() {
    let a: SensitiveData<SomeTestStruct> = SensitiveData::new(SomeTestStruct { a: 5 });
    assert_eq!(a.borrow().a, 5);
  }

  #[test]
  fn destructor_executed() {
    let mut a: SensitiveData<WithDestructor> = unsafe { SensitiveData::new_zeroed() };
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
    let a: SensitiveData<SomeTestStruct> = unsafe { SensitiveData::new_zeroed() };
    let _b = a.borrow();
    let _c = a.borrow();
  }
  #[test]
  fn reader_then_writer_then_reader() {
    let mut a: SensitiveData<SomeTestStruct> = unsafe { SensitiveData::new_zeroed() };
    {
      let _b = a.borrow();
    }
    {
      let _c = a.borrow_mut();
    }
    let _c = a.borrow();
  }
}
