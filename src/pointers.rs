//! Pointer Types
//! 
//! Since fusion blossom requires no synchronization with mutex, it's inefficient to wrap everything in a mutex.
//! At the same time, I want to enjoy the safety check provided by Rust compiler, so I want to limit unsafe code to minimum.
//! The solution is to write everything in safe Rust, and debug them.
//! After this, one can enable the feature `unsafe_pointer` to remove the unnecessary locks, thus improving the performance.
//! 


use std::sync::{Arc, Weak};
use crate::parking_lot::{RwLock, RawRwLock};
use crate::parking_lot::lock_api::{RwLockReadGuard, RwLockWriteGuard};
use super::util::*;


/// allows fast reset of vector of objects without iterating over all objects each time: dynamically clear it
pub trait FastClear {

    /// user provided method to actually clear the fields
    fn hard_clear(&mut self);

    /// get timestamp
    fn get_timestamp(&self) -> FastClearTimestamp;

    /// set timestamp
    fn set_timestamp(&mut self, timestamp: FastClearTimestamp);

    /// dynamically clear it if not already cleared; it's safe to call many times
    #[inline(always)]
    fn dynamic_clear(&mut self, active_timestamp: FastClearTimestamp) {
        if self.get_timestamp() != active_timestamp {
            self.hard_clear();
            self.set_timestamp(active_timestamp);
        }
    }

    /// when debugging your program, you can put this function every time you obtained a lock of a new object
    #[inline(always)]
    fn debug_assert_dynamic_cleared(&self, active_timestamp: FastClearTimestamp) {
        debug_assert!(self.get_timestamp() == active_timestamp, "bug detected: not dynamically cleared, expected timestamp: {}, current timestamp: {}"
            , active_timestamp, self.get_timestamp());
    }

}

pub trait FastClearRwLockPtr<ObjType> where ObjType: FastClear {

    fn new_ptr(ptr: Arc<RwLock<ObjType>>) -> Self;

    fn new_value(obj: ObjType) -> Self;

    fn ptr(&self) -> &Arc<RwLock<ObjType>>;

    fn ptr_mut(&mut self) -> &mut Arc<RwLock<ObjType>>;

    #[inline(always)]
    fn read_recursive(&self, active_timestamp: FastClearTimestamp) -> RwLockReadGuard<RawRwLock, ObjType> {
        let ret = self.ptr().read_recursive();
        ret.debug_assert_dynamic_cleared(active_timestamp);  // only assert during debug modes
        ret
    }

    /// without sanity check: this data might be outdated, so only use when you're read those immutable fields 
    #[inline(always)]
    fn read_recursive_force(&self) -> RwLockReadGuard<RawRwLock, ObjType> {
        let ret = self.ptr().read_recursive();
        ret
    }

    #[inline(always)]
    fn write(&self, active_timestamp: FastClearTimestamp) -> RwLockWriteGuard<RawRwLock, ObjType> {
        let ret = self.ptr().write();
        ret.debug_assert_dynamic_cleared(active_timestamp);  // only assert during debug modes
        ret
    }

    /// without sanity check: useful only in implementing hard_clear
    #[inline(always)]
    fn write_force(&self) -> RwLockWriteGuard<RawRwLock, ObjType> {
        let ret = self.ptr().write();
        ret
    }

    /// dynamically clear it if not already cleared; it's safe to call many times, but it will acquire a writer lock
    #[inline(always)]
    fn dynamic_clear(&self, active_timestamp: FastClearTimestamp) {
        let mut value = self.write_force();
        value.dynamic_clear(active_timestamp);
    }

    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.ptr(), other.ptr())
    }

}

pub trait RwLockPtr<ObjType> {

    fn new_ptr(ptr: Arc<RwLock<ObjType>>) -> Self;

    fn new_value(obj: ObjType) -> Self;

    fn ptr(&self) -> &Arc<RwLock<ObjType>>;

    fn ptr_mut(&mut self) -> &mut Arc<RwLock<ObjType>>;

    #[inline(always)]
    fn read_recursive(&self) -> RwLockReadGuard<RawRwLock, ObjType> {
        let ret = self.ptr().read_recursive();
        ret
    }

    #[inline(always)]
    fn write(&self) -> RwLockWriteGuard<RawRwLock, ObjType> {
        let ret = self.ptr().write();
        ret
    }

    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.ptr(), other.ptr())
    }

}

pub struct ArcRwLock<T> {
    ptr: Arc<RwLock<T>>,
}

pub struct WeakRwLock<T> {
    ptr: Weak<RwLock<T>>,
}

impl<T> ArcRwLock<T> {
    pub fn downgrade(&self) -> WeakRwLock<T> {
        WeakRwLock::<T> {
            ptr: Arc::downgrade(&self.ptr)
        }
    }
}

impl<T> WeakRwLock<T> {
    pub fn upgrade_force(&self) -> ArcRwLock<T> {
        ArcRwLock::<T> {
            ptr: self.ptr.upgrade().unwrap()
        }
    }
    pub fn upgrade(&self) -> Option<ArcRwLock<T>> {
        self.ptr.upgrade().map(|x| ArcRwLock::<T> { ptr: x })
    }
}

impl<T> Clone for ArcRwLock<T> {
    fn clone(&self) -> Self {
        Self::new_ptr(Arc::clone(self.ptr()))
    }
}

impl<T> RwLockPtr<T> for ArcRwLock<T> {
    fn new_ptr(ptr: Arc<RwLock<T>>) -> Self { Self { ptr }  }
    fn new_value(obj: T) -> Self { Self::new_ptr(Arc::new(RwLock::new(obj))) }
    #[inline(always)] fn ptr(&self) -> &Arc<RwLock<T>> { &self.ptr }
    #[inline(always)] fn ptr_mut(&mut self) -> &mut Arc<RwLock<T>> { &mut self.ptr }
}

impl<T> PartialEq for ArcRwLock<T> {
    fn eq(&self, other: &Self) -> bool { self.ptr_eq(other) }
}

impl<T> Eq for ArcRwLock<T> { }

impl<T> Clone for WeakRwLock<T> {
    fn clone(&self) -> Self {
        Self { ptr: self.ptr.clone() }
    }
}

impl<T> PartialEq for WeakRwLock<T> {
    fn eq(&self, other: &Self) -> bool { self.ptr.ptr_eq(&other.ptr) }
}

impl<T> Eq for WeakRwLock<T> { }

impl<T> std::ops::Deref for ArcRwLock<T> {
    type Target = RwLock<T>;
    fn deref(&self) -> &Self::Target {
        &self.ptr
    }
}

impl<T> weak_table::traits::WeakElement for WeakRwLock<T> {
    type Strong = ArcRwLock<T>;
    fn new(view: &Self::Strong) -> Self {
        view.downgrade()
    }
    fn view(&self) -> Option<Self::Strong> {
        self.upgrade()
    }
    fn clone(view: &Self::Strong) -> Self::Strong {
        view.clone()
    }
}

pub struct FastClearArcRwLock<T: FastClear> {
    ptr: Arc<RwLock<T>>,
}

pub struct FastClearWeakRwLock<T: FastClear> {
    ptr: Weak<RwLock<T>>,
}

impl<T: FastClear> FastClearArcRwLock<T> {
    pub fn downgrade(&self) -> FastClearWeakRwLock<T> {
        FastClearWeakRwLock::<T> {
            ptr: Arc::downgrade(&self.ptr)
        }
    }
}

impl<T: FastClear> FastClearWeakRwLock<T> {
    pub fn upgrade_force(&self) -> FastClearArcRwLock<T> {
        FastClearArcRwLock::<T> {
            ptr: self.ptr.upgrade().unwrap()
        }
    }
    pub fn upgrade(&self) -> Option<FastClearArcRwLock<T>> {
        self.ptr.upgrade().map(|x| FastClearArcRwLock::<T> { ptr: x })
    }
}

impl<T: FastClear> Clone for FastClearArcRwLock<T> {
    fn clone(&self) -> Self {
        Self::new_ptr(Arc::clone(self.ptr()))
    }
}

impl<T: FastClear> FastClearRwLockPtr<T> for FastClearArcRwLock<T> {
    fn new_ptr(ptr: Arc<RwLock<T>>) -> Self { Self { ptr }  }
    fn new_value(obj: T) -> Self { Self::new_ptr(Arc::new(RwLock::new(obj))) }
    #[inline(always)] fn ptr(&self) -> &Arc<RwLock<T>> { &self.ptr }
    #[inline(always)] fn ptr_mut(&mut self) -> &mut Arc<RwLock<T>> { &mut self.ptr }
}

impl<T: FastClear> PartialEq for FastClearArcRwLock<T> {
    fn eq(&self, other: &Self) -> bool { self.ptr_eq(other) }
}

impl<T: FastClear> Eq for FastClearArcRwLock<T> { }

impl<T: FastClear> Clone for FastClearWeakRwLock<T> {
    fn clone(&self) -> Self {
        Self { ptr: self.ptr.clone() }
    }
}

impl<T: FastClear> PartialEq for FastClearWeakRwLock<T> {
    fn eq(&self, other: &Self) -> bool { self.ptr.ptr_eq(&other.ptr) }
}

impl<T: FastClear> Eq for FastClearWeakRwLock<T> { }

impl<T: FastClear> std::ops::Deref for FastClearArcRwLock<T> {
    type Target = RwLock<T>;
    fn deref(&self) -> &Self::Target {
        &self.ptr
    }
}

impl<T: FastClear> weak_table::traits::WeakElement for FastClearWeakRwLock<T> {
    type Strong = FastClearArcRwLock<T>;
    fn new(view: &Self::Strong) -> Self {
        view.downgrade()
    }
    fn view(&self) -> Option<Self::Strong> {
        self.upgrade()
    }
    fn clone(view: &Self::Strong) -> Self::Strong {
        view.clone()
    }
}

  
/*
 * unsafe APIs, used for production environment where speed matters
 */

cfg_if::cfg_if! {
    if #[cfg(feature="unsafe_pointer")] {

        pub trait FastClearUnsafePtr<ObjType> where ObjType: FastClear {

            fn new_ptr(ptr: Arc<ObjType>) -> Self;

            fn new_value(obj: ObjType) -> Self;

            fn ptr(&self) -> &Arc<ObjType>;

            fn ptr_mut(&mut self) -> &mut Arc<ObjType>;

            #[inline(always)]
            fn read_recursive(&self, active_timestamp: FastClearTimestamp) -> &ObjType {
                let ret = self.ptr();
                ret.debug_assert_dynamic_cleared(active_timestamp);  // only assert during debug modes
                ret
            }

            /// without sanity check: this data might be outdated, so only use when you're read those immutable fields 
            #[inline(always)]
            fn read_recursive_force(&self) -> &ObjType {
                self.ptr()
            }

            #[inline(always)]
            fn write(&self, active_timestamp: FastClearTimestamp) -> &mut ObjType {
                unsafe {
                    // https://stackoverflow.com/questions/54237610/is-there-a-way-to-make-an-immutable-reference-mutable
                    let ptr = self.ptr();
                    let const_ptr = ptr as *const Arc<ObjType>;
                    let mut_ptr = const_ptr as *mut Arc<ObjType>;
                    let ret = Arc::get_mut_unchecked(&mut *mut_ptr);
                    ret.debug_assert_dynamic_cleared(active_timestamp);  // only assert during debug modes
                    ret
                }
            }

            /// without sanity check: useful only in implementing hard_clear
            #[inline(always)]
            fn write_force(&self) -> &mut ObjType {
                unsafe {
                    // https://stackoverflow.com/questions/54237610/is-there-a-way-to-make-an-immutable-reference-mutable
                    let ptr = self.ptr();
                    let const_ptr = ptr as *const Arc<ObjType>;
                    let mut_ptr = const_ptr as *mut Arc<ObjType>;
                    Arc::get_mut_unchecked(&mut *mut_ptr)
                }
            }

            /// dynamically clear it if not already cleared; it's safe to call many times, but it will acquire a writer lock
            #[inline(always)]
            fn dynamic_clear(&self, active_timestamp: FastClearTimestamp) {
                let value = self.write_force();
                value.dynamic_clear(active_timestamp);
            }

            fn ptr_eq(&self, other: &Self) -> bool {
                Arc::ptr_eq(self.ptr(), other.ptr())
            }

        }

        pub trait UnsafePtr<ObjType> {

            fn new_ptr(ptr: Arc<ObjType>) -> Self;

            fn new_value(obj: ObjType) -> Self;

            fn ptr(&self) -> &Arc<ObjType>;

            fn ptr_mut(&mut self) -> &mut Arc<ObjType>;

            #[inline(always)]
            fn read_recursive(&self) -> &ObjType {
                self.ptr()
            }

            #[inline(always)]
            fn write(&self) -> &mut ObjType {
                unsafe {
                    // https://stackoverflow.com/questions/54237610/is-there-a-way-to-make-an-immutable-reference-mutable
                    let ptr = self.ptr();
                    let const_ptr = ptr as *const Arc<ObjType>;
                    let mut_ptr = const_ptr as *mut Arc<ObjType>;
                    Arc::get_mut_unchecked(&mut *mut_ptr)
                }
            }

            fn ptr_eq(&self, other: &Self) -> bool {
                Arc::ptr_eq(self.ptr(), other.ptr())
            }

        }

        pub struct ArcUnsafe<T> {
            ptr: Arc<T>,
        }

        pub struct WeakUnsafe<T> {
            ptr: Weak<T>,
        }

        impl<T> ArcUnsafe<T> {
            pub fn downgrade(&self) -> WeakUnsafe<T> {
                WeakUnsafe::<T> {
                    ptr: Arc::downgrade(&self.ptr)
                }
            }
        }

        impl<T> WeakUnsafe<T> {
            pub fn upgrade_force(&self) -> ArcUnsafe<T> {
                ArcUnsafe::<T> {
                    ptr: self.ptr.upgrade().unwrap()
                }
            }
            pub fn upgrade(&self) -> Option<ArcUnsafe<T>> {
                self.ptr.upgrade().map(|x| ArcUnsafe::<T> { ptr: x })
            }
        }

        impl<T> Clone for ArcUnsafe<T> {
            fn clone(&self) -> Self {
                Self::new_ptr(Arc::clone(self.ptr()))
            }
        }

        impl<T> UnsafePtr<T> for ArcUnsafe<T> {
            fn new_ptr(ptr: Arc<T>) -> Self { Self { ptr }  }
            fn new_value(obj: T) -> Self { Self::new_ptr(Arc::new(obj)) }
            #[inline(always)] fn ptr(&self) -> &Arc<T> { &self.ptr }
            #[inline(always)] fn ptr_mut(&mut self) -> &mut Arc<T> { &mut self.ptr }
        }

        impl<T> PartialEq for ArcUnsafe<T> {
            fn eq(&self, other: &Self) -> bool { self.ptr_eq(other) }
        }

        impl<T> Eq for ArcUnsafe<T> { }

        impl<T> Clone for WeakUnsafe<T> {
            fn clone(&self) -> Self {
                Self { ptr: self.ptr.clone() }
            }
        }

        impl<T> PartialEq for WeakUnsafe<T> {
            fn eq(&self, other: &Self) -> bool { self.ptr.ptr_eq(&other.ptr) }
        }

        impl<T> Eq for WeakUnsafe<T> { }

        impl<T> std::ops::Deref for ArcUnsafe<T> {
            type Target = T;
            fn deref(&self) -> &Self::Target {
                &self.ptr
            }
        }

        impl<T> weak_table::traits::WeakElement for WeakUnsafe<T> {
            type Strong = ArcUnsafe<T>;
            fn new(view: &Self::Strong) -> Self {
                view.downgrade()
            }
            fn view(&self) -> Option<Self::Strong> {
                self.upgrade()
            }
            fn clone(view: &Self::Strong) -> Self::Strong {
                view.clone()
            }
        }

        pub struct FastClearArcUnsafe<T: FastClear> {
            ptr: Arc<T>,
        }

        pub struct FastClearWeakUnsafe<T: FastClear> {
            ptr: Weak<T>,
        }

        impl<T: FastClear> FastClearArcUnsafe<T> {
            pub fn downgrade(&self) -> FastClearWeakUnsafe<T> {
                FastClearWeakUnsafe::<T> {
                    ptr: Arc::downgrade(&self.ptr)
                }
            }
        }

        impl<T: FastClear> FastClearWeakUnsafe<T> {
            pub fn upgrade_force(&self) -> FastClearArcUnsafe<T> {
                FastClearArcUnsafe::<T> {
                    ptr: self.ptr.upgrade().unwrap()
                }
            }
            pub fn upgrade(&self) -> Option<FastClearArcUnsafe<T>> {
                self.ptr.upgrade().map(|x| FastClearArcUnsafe::<T> { ptr: x })
            }
        }

        impl<T: FastClear> Clone for FastClearArcUnsafe<T> {
            fn clone(&self) -> Self {
                Self::new_ptr(Arc::clone(self.ptr()))
            }
        }

        impl<T: FastClear> FastClearUnsafePtr<T> for FastClearArcUnsafe<T> {
            fn new_ptr(ptr: Arc<T>) -> Self { Self { ptr }  }
            fn new_value(obj: T) -> Self { Self::new_ptr(Arc::new(obj)) }
            #[inline(always)] fn ptr(&self) -> &Arc<T> { &self.ptr }
            #[inline(always)] fn ptr_mut(&mut self) -> &mut Arc<T> { &mut self.ptr }
        }

        impl<T: FastClear> PartialEq for FastClearArcUnsafe<T> {
            fn eq(&self, other: &Self) -> bool { self.ptr_eq(other) }
        }

        impl<T: FastClear> Eq for FastClearArcUnsafe<T> { }

        impl<T: FastClear> Clone for FastClearWeakUnsafe<T> {
            fn clone(&self) -> Self {
                Self { ptr: self.ptr.clone() }
            }
        }

        impl<T: FastClear> PartialEq for FastClearWeakUnsafe<T> {
            fn eq(&self, other: &Self) -> bool { self.ptr.ptr_eq(&other.ptr) }
        }

        impl<T: FastClear> Eq for FastClearWeakUnsafe<T> { }

        impl<T: FastClear> std::ops::Deref for FastClearArcUnsafe<T> {
            type Target = T;
            fn deref(&self) -> &Self::Target {
                &self.ptr
            }
        }

        impl<T: FastClear> weak_table::traits::WeakElement for FastClearWeakUnsafe<T> {
            type Strong = FastClearArcUnsafe<T>;
            fn new(view: &Self::Strong) -> Self {
                view.downgrade()
            }
            fn view(&self) -> Option<Self::Strong> {
                self.upgrade()
            }
            fn clone(view: &Self::Strong) -> Self::Strong {
                view.clone()
            }
        }

    }
    
}

cfg_if::cfg_if! {
    if #[cfg(feature="unsafe_arc")] {

        pub trait FastClearUnsafePtrDangerous<ObjType> where ObjType: FastClear {

            fn new_ptr(ptr: Arc<ObjType>) -> Self;

            fn new_value(obj: ObjType) -> Self;

            fn ptr(&self) -> *const ObjType;

            #[inline(always)]
            fn read_recursive(&self, active_timestamp: FastClearTimestamp) -> &ObjType {
                unsafe {
                    let ret = &*self.ptr();
                    ret.debug_assert_dynamic_cleared(active_timestamp);  // only assert during debug modes
                    ret
                }
            }

            /// without sanity check: this data might be outdated, so only use when you're read those immutable fields 
            #[inline(always)]
            fn read_recursive_force(&self) -> &ObjType {
                unsafe {
                    &*self.ptr()
                }
            }

            #[inline(always)]
            fn write(&self, active_timestamp: FastClearTimestamp) -> &mut ObjType {
                unsafe {
                    // https://stackoverflow.com/questions/54237610/is-there-a-way-to-make-an-immutable-reference-mutable
                    let const_ptr = self.ptr();
                    let mut_ptr = &mut *(const_ptr as *mut ObjType);
                    mut_ptr.debug_assert_dynamic_cleared(active_timestamp);  // only assert during debug modes
                    mut_ptr
                }
            }

            /// without sanity check: useful only in implementing hard_clear
            #[inline(always)]
            fn write_force(&self) -> &mut ObjType {
                unsafe {
                    // https://stackoverflow.com/questions/54237610/is-there-a-way-to-make-an-immutable-reference-mutable
                    let const_ptr = self.ptr();
                    let mut_ptr = const_ptr as *mut ObjType;
                    &mut *mut_ptr
                }
            }

            /// dynamically clear it if not already cleared; it's safe to call many times, but it will acquire a writer lock
            #[inline(always)]
            fn dynamic_clear(&self, active_timestamp: FastClearTimestamp) {
                let value = self.write_force();
                value.dynamic_clear(active_timestamp);
            }

            #[inline(always)]
            fn ptr_eq(&self, other: &Self) -> bool {
                std::ptr::eq(self.ptr(), other.ptr())
            }

        }

        pub trait UnsafePtrDangerous<ObjType> {

            fn new_ptr(ptr: Arc<ObjType>) -> Self;

            fn new_value(obj: ObjType) -> Self;

            fn ptr(&self) -> *const ObjType;

            #[inline(always)]
            fn read_recursive(&self) -> &ObjType {
                unsafe {
                    &*self.ptr()
                }
            }

            #[inline(always)]
            fn write(&self) -> &mut ObjType {
                unsafe {
                    // https://stackoverflow.com/questions/54237610/is-there-a-way-to-make-an-immutable-reference-mutable
                    let const_ptr = self.ptr();
                    let mut_ptr = const_ptr as *mut ObjType;
                    &mut *mut_ptr
                }
            }

            #[inline(always)]
            fn ptr_eq(&self, other: &Self) -> bool {
                std::ptr::eq(self.ptr(), other.ptr())
            }

        }

        pub enum ArcUnsafeDangerous<T> {
            Owner(Arc<T>),
            Ref(*const T),
        }

        pub struct WeakUnsafeDangerous<T> {
            ptr: *const T,
        }

        unsafe impl<T> Send for ArcUnsafeDangerous<T> {}
        unsafe impl<T> Sync for ArcUnsafeDangerous<T> {}

        unsafe impl<T> Send for WeakUnsafeDangerous<T> {}
        unsafe impl<T> Sync for WeakUnsafeDangerous<T> {}

        impl<T> ArcUnsafeDangerous<T> {
            #[inline(always)]
            pub fn downgrade(&self) -> WeakUnsafeDangerous<T> {
                WeakUnsafeDangerous::<T> {
                    ptr: self.ptr()
                }
            }
        }

        impl<T> WeakUnsafeDangerous<T> {
            #[inline(always)]
            pub fn upgrade_force(&self) -> ArcUnsafeDangerous<T> {
                ArcUnsafeDangerous::<T>::Ref(self.ptr)
            }
        }

        impl<T> Clone for ArcUnsafeDangerous<T> {
            #[inline(always)]
            fn clone(&self) -> Self {
                Self::Ref(self.ptr())
            }
        }

        impl<T> UnsafePtrDangerous<T> for ArcUnsafeDangerous<T> {
            fn new_ptr(ptr: Arc<T>) -> Self { Self::Owner(ptr)  }
            fn new_value(obj: T) -> Self { Self::Owner(Arc::new(obj)) }
            #[inline(always)]
            fn ptr(&self) -> *const T {
                match self {
                    Self::Owner(ptr) => Arc::as_ptr(ptr),
                    Self::Ref(ptr) => *ptr,
                }
            }
        }

        impl<T> PartialEq for ArcUnsafeDangerous<T> {
            #[inline(always)]
            fn eq(&self, other: &Self) -> bool { self.ptr_eq(other) }
        }

        impl<T> Eq for ArcUnsafeDangerous<T> { }

        impl<T> Clone for WeakUnsafeDangerous<T> {
            #[inline(always)]
            fn clone(&self) -> Self {
                Self { ptr: self.ptr.clone() }
            }
        }

        impl<T> PartialEq for WeakUnsafeDangerous<T> {
            #[inline(always)]
            fn eq(&self, other: &Self) -> bool { std::ptr::eq(self.ptr, other.ptr) }
        }

        impl<T> Eq for WeakUnsafeDangerous<T> { }

        impl<T> std::ops::Deref for ArcUnsafeDangerous<T> {
            type Target = T;
            #[inline(always)]
            fn deref(&self) -> &Self::Target {
                unsafe {
                    match self {
                        Self::Owner(ptr) => &ptr,
                        Self::Ref(ptr) => &**ptr,
                    }
                }
            }
        }

        impl<T> weak_table::traits::WeakElement for WeakUnsafeDangerous<T> {
            type Strong = ArcUnsafeDangerous<T>;
            #[inline(always)]
            fn new(view: &Self::Strong) -> Self {
                view.downgrade()
            }
            #[inline(always)]
            fn view(&self) -> Option<Self::Strong> {
                Some(self.upgrade_force())
            }
            #[inline(always)]
            fn clone(view: &Self::Strong) -> Self::Strong {
                view.clone()
            }
        }

        pub enum FastClearArcUnsafeDangerous<T: FastClear> {
            Owner(Arc<T>),
            Ref(*const T),
        }

        pub struct FastClearWeakUnsafeDangerous<T: FastClear> {
            ptr: *const T,
        }

        unsafe impl<T: FastClear> Send for FastClearArcUnsafeDangerous<T> {}
        unsafe impl<T: FastClear> Sync for FastClearArcUnsafeDangerous<T> {}
    
        unsafe impl<T: FastClear> Send for FastClearWeakUnsafeDangerous<T> {}
        unsafe impl<T: FastClear> Sync for FastClearWeakUnsafeDangerous<T> {}

        impl<T: FastClear> FastClearArcUnsafeDangerous<T> {
            #[inline(always)]
            pub fn downgrade(&self) -> FastClearWeakUnsafeDangerous<T> {
                FastClearWeakUnsafeDangerous::<T> {
                    ptr: self.ptr()
                }
            }
        }

        impl<T: FastClear> FastClearWeakUnsafeDangerous<T> {
            #[inline(always)]
            pub fn upgrade_force(&self) -> FastClearArcUnsafeDangerous<T> {
                FastClearArcUnsafeDangerous::<T>::Ref(self.ptr)
            }
        }

        impl<T: FastClear> Clone for FastClearArcUnsafeDangerous<T> {
            #[inline(always)]
            fn clone(&self) -> Self {
                Self::Ref(self.ptr())
            }
        }

        impl<T: FastClear> FastClearUnsafePtrDangerous<T> for FastClearArcUnsafeDangerous<T> {
            fn new_ptr(ptr: Arc<T>) -> Self { Self::Owner(ptr) }
            fn new_value(obj: T) -> Self { Self::Owner(Arc::new(obj)) }
            #[inline(always)]
            fn ptr(&self) -> *const T {
                match self {
                    Self::Owner(ptr) => Arc::as_ptr(ptr),
                    Self::Ref(ptr) => *ptr,
                }
            }
        }

        impl<T: FastClear> PartialEq for FastClearArcUnsafeDangerous<T> {
            #[inline(always)]
            fn eq(&self, other: &Self) -> bool { self.ptr_eq(other) }
        }

        impl<T: FastClear> Eq for FastClearArcUnsafeDangerous<T> { }

        impl<T: FastClear> Clone for FastClearWeakUnsafeDangerous<T> {
            #[inline(always)]
            fn clone(&self) -> Self {
                Self { ptr: self.ptr.clone() }
            }
        }

        impl<T: FastClear> PartialEq for FastClearWeakUnsafeDangerous<T> {
            #[inline(always)]
            fn eq(&self, other: &Self) -> bool { std::ptr::eq(self.ptr, other.ptr) }
        }

        impl<T: FastClear> Eq for FastClearWeakUnsafeDangerous<T> { }

        impl<T: FastClear> std::ops::Deref for FastClearArcUnsafeDangerous<T> {
            type Target = T;
            #[inline(always)]
            fn deref(&self) -> &Self::Target {
                unsafe {
                    match self {
                        Self::Owner(ptr) => &ptr,
                        Self::Ref(ptr) => &**ptr,
                    }
                }
            }
        }

        impl<T: FastClear> weak_table::traits::WeakElement for FastClearWeakUnsafeDangerous<T> {
            type Strong = FastClearArcUnsafeDangerous<T>;
            #[inline(always)]
            fn new(view: &Self::Strong) -> Self {
                view.downgrade()
            }
            #[inline(always)]
            fn view(&self) -> Option<Self::Strong> {
                Some(self.upgrade_force())
            }
            #[inline(always)]
            fn clone(view: &Self::Strong) -> Self::Strong {
                view.clone()
            }
        }

    }
}

cfg_if::cfg_if! {
    if #[cfg(feature="unsafe_pointer")] {
        pub type FastClearArcManualSafeLock<T> = FastClearArcUnsafe<T>;
        pub type FastClearWeakManualSafeLock<T> = FastClearWeakUnsafe<T>;
        pub type ArcManualSafeLock<T> = ArcUnsafe<T>;
        pub type WeakManualSafeLock<T> = WeakUnsafe<T>;
        #[macro_export]
        macro_rules! lock_write {
            ($variable:ident, $lock:expr) => { let $variable = $lock.write(); };
            ($variable:ident, $lock:expr, $timestamp:expr) => { let $variable = $lock.write($timestamp); };
        }
        #[allow(unused_imports)] pub use lock_write;
        cfg_if::cfg_if! {
            if #[cfg(feature="unsafe_arc")] {
                pub type FastClearArcManualSafeLockDangerous<T> = FastClearArcUnsafeDangerous<T>;
                pub type FastClearWeakManualSafeLockDangerous<T> = FastClearWeakUnsafeDangerous<T>;
                pub type ArcManualSafeLockDangerous<T> = ArcUnsafeDangerous<T>;
                pub type WeakManualSafeLockDangerous<T> = WeakUnsafeDangerous<T>;
            } else {
                pub type FastClearArcManualSafeLockDangerous<T> = FastClearArcUnsafe<T>;
                pub type FastClearWeakManualSafeLockDangerous<T> = FastClearWeakUnsafe<T>;
                pub type ArcManualSafeLockDangerous<T> = ArcUnsafe<T>;
                pub type WeakManualSafeLockDangerous<T> = WeakUnsafe<T>;
            }
        }
    } else {
        pub type FastClearArcManualSafeLock<T> = FastClearArcRwLock<T>;
        pub type FastClearWeakManualSafeLock<T> = FastClearWeakRwLock<T>;
        pub type ArcManualSafeLock<T> = ArcRwLock<T>;
        pub type WeakManualSafeLock<T> = WeakRwLock<T>;
        #[macro_export]
        macro_rules! lock_write {
            ($variable:ident, $lock:expr) => { let mut $variable = $lock.write(); };
            ($variable:ident, $lock:expr, $timestamp:expr) => { let mut $variable = $lock.write($timestamp); };
        }
        #[allow(unused_imports)] pub use lock_write;
        pub type FastClearArcManualSafeLockDangerous<T> = FastClearArcRwLock<T>;
        pub type FastClearWeakManualSafeLockDangerous<T> = FastClearWeakRwLock<T>;
        pub type ArcManualSafeLockDangerous<T> = ArcRwLock<T>;
        pub type WeakManualSafeLockDangerous<T> = WeakRwLock<T>;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Tester {
        idx: usize,
    }

    type TesterPtr = ArcRwLock<Tester>;
    type TesterWeak = WeakRwLock<Tester>;

    impl std::fmt::Debug for TesterPtr {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            let value = self.read_recursive();
            write!(f, "{:?}", value)
        }
    }

    impl std::fmt::Debug for TesterWeak {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            self.upgrade_force().fmt(f)
        }
    }

    #[test]
    fn pointers_test_1() {  // cargo test pointers_test_1 -- --nocapture
        let ptr = TesterPtr::new_value(Tester { idx: 0 });
        let weak = ptr.downgrade();
        ptr.write().idx = 1;
        assert_eq!(weak.upgrade_force().read_recursive().idx, 1);
        weak.upgrade_force().write().idx = 2;
        assert_eq!(ptr.read_recursive().idx, 2);
    }

cfg_if::cfg_if! {
    if #[cfg(feature="unsafe_pointer")] {

        type TesterUnsafePtr = ArcRwLock<Tester>;

        #[test]
        fn pointers_test_2() {  // cargo test pointers_test_2 --features unsafe_pointer -- --nocapture
            let ptr = TesterUnsafePtr::new_value(Tester { idx: 0 });
            let weak = ptr.downgrade();
            ptr.write().idx = 1;
            assert_eq!(weak.upgrade_force().read_recursive().idx, 1);
            weak.upgrade_force().write().idx = 2;
            assert_eq!(ptr.read_recursive().idx, 2);
        }

    }
}


cfg_if::cfg_if! {
    if #[cfg(feature="unsafe_arc")] {

        #[test]
        fn pointers_test_3() {  // cargo test pointers_test_3 --features unsafe_arc -- --nocapture
            println!("{}", std::mem::size_of::<ArcManualSafeLock<Tester>>());
            println!("{}", std::mem::size_of::<ArcManualSafeLockDangerous<Tester>>());
            println!("{}", std::mem::size_of::<Arc<Tester>>());
            println!("{}", std::mem::size_of::<*const Tester>());
        }

    }
}

}
