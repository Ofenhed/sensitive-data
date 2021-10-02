use std::{cell::Cell, ops::{Deref, DerefMut}, marker::PhantomPinned};

struct HolderInner<T> {
  value: T,
  _marker: PhantomPinned,
}

pub struct SensitiveData<T> {
    inner_size: usize,
    block_size: usize,
    inner_ptr: *mut HolderInner<T>,
    derefs: Cell<usize>,
}

pub struct DerefHolder<'holder, T> {
    holder: &'holder SensitiveData<T>,
}

pub struct DerefMutHolder<'holder, T> {
    holder: &'holder mut SensitiveData<T>,
}

impl<T> Drop for DerefMutHolder<'_, T> {
    fn drop(&mut self) {
        self.holder.make_inaccessible();
        println!("Drop mut derefholder");
    }
}

impl<T> Drop for DerefHolder<'_, T> {
    fn drop(&mut self) {
        let val_before = self.holder.derefs.get();
        let new_value = val_before - 1;
        if val_before != self.holder.derefs.replace(new_value) {
          panic!("Failed race for DerefHolder");
        }
        if new_value == 0 {
          self.holder.make_inaccessible();
        }
        println!("Drop derefholder");
    }
}

impl<T> Drop for SensitiveData<T> {
    fn drop(&mut self) {
        self.make_writable();
        unsafe { std::ptr::drop_in_place(self.inner_ptr); }
        self.zeroize_inner();
        println!("Drop holder");
    }
}

impl<'deref_holder, T> Deref for DerefHolder<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.holder.make_readable();
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
    pub fn new_zeroed() -> Self {
      use std::{alloc::{alloc, Layout}, mem::size_of};
      let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
      let object_size = size_of::<HolderInner<T>>();
      let block_size = object_size / page_size;
      let block_size = if block_size < object_size { block_size + page_size } else { block_size };
      let memory_layout = Layout::from_size_align(block_size, page_size).unwrap_or_else(|_| panic!("Invalid pagesize: {}", page_size));
      let inner_ptr;
      unsafe {
        let allocated = alloc(memory_layout);
        inner_ptr = allocated as *mut HolderInner<T>;
        libc::mlock(inner_ptr as *mut libc::c_void, block_size);
      }
      let mut holder = SensitiveData { inner_size: object_size, block_size, inner_ptr, derefs: Cell::new(0) };
      holder.zeroize_inner();
      holder.make_inaccessible();
      holder
    }

    #[inline(always)]
    fn zeroize<const BYTES_ZEROIZED: usize>(&mut self, offset: isize) {
      use std::ptr::write_volatile;
      println!("Zeroized {} bytes at offset {} (total {} bytes)", BYTES_ZEROIZED, offset, self.inner_size);
      unsafe { write_volatile((self.inner_ptr as *mut [u8; BYTES_ZEROIZED]).offset(offset), [0u8; BYTES_ZEROIZED]) };
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
      println!("Is inaccessible");
      if unsafe { libc::mprotect(self.inner_ptr as *mut libc::c_void, self.block_size, libc::PROT_NONE) } != 0 {
        panic!("Could not make memory inaccessible");
      }
    }

    #[cfg(target_family = "unix")]
    fn make_readable(&self) {
      println!("Is readable");
      if unsafe { libc::mprotect(self.inner_ptr as *mut libc::c_void, self.block_size, libc::PROT_READ) } != 0 {
        panic!("Could not make memory read only");
      }
    }

    #[cfg(target_family = "unix")]
    fn make_writable(&mut self) {
      println!("Is writable");
      if unsafe { libc::mprotect(self.inner_ptr as *mut libc::c_void, self.block_size, libc::PROT_READ | libc::PROT_WRITE) } != 0 {
        panic!("Could not make memory inaccessible");
      }
    }

    #[inline(always)]
    pub fn borrow(&self) -> DerefHolder<T> {
        let val_before = self.derefs.get();
        if val_before != self.derefs.replace(val_before + 1) {
          panic!("Failed race for DerefHolder");
        }
        DerefHolder { holder: self }
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
      unsafe { *self.destructor_executed = true; }
    }
  }

  #[test]
  fn zeroized_when_created() {
    let a: SensitiveData<SomeTestStruct> = SensitiveData::new_zeroed();
    assert_eq!(a.borrow().a, 0);
  }

  #[test]
  fn destructor_executed() {
    let mut a: SensitiveData<WithDestructor> = SensitiveData::new_zeroed();
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
    let a: SensitiveData<SomeTestStruct> = SensitiveData::new_zeroed();
    let _b = a.borrow();
    let _c = a.borrow();
  }
  #[test]
  fn reader_then_writer_then_reader() {
    let mut a: SensitiveData<SomeTestStruct> = SensitiveData::new_zeroed();
    {
      let _b = a.borrow();
    }
    {
      let _c = a.borrow_mut();
    }
    let _c = a.borrow();
  }
}