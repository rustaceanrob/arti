//! Helpers for working with FFI.

use std::{
    ffi::{c_char, CStr},
    mem::MaybeUninit,
};

use super::err::InvalidInput;

/// Try to convert a `const char*` from C to a Rust `&str`, but return an error if the pointer
/// is NULL or not UTF8.
///
/// # Safety
///
/// See [`CStr::from_ptr`].  All those restrictions apply, except that we tolerate a NULL pointer.
/// These rules are precisely those in the Arti RPC FFI for a `const char*` passed in from C.
pub(super) unsafe fn ptr_to_str<'a>(p: *const c_char) -> Result<&'a str, InvalidInput> {
    if p.is_null() {
        return Err(InvalidInput::NullPointer);
    }

    // Safety: We require that the safety properties of CStr::from_ptr hold.
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| InvalidInput::BadUtf8)
}

/// If `p` is non-null, convert it into a `Box<T>`.  Otherwise return None.
///
/// # Safety
///
/// If `p` is non-NULL, it must point to a valid instance of T that was constructed
/// using `Box::into_raw(Box::new(..))`.
pub(super) unsafe fn ptr_to_opt_box<T>(p: *mut T) -> Option<Box<T>> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { Box::from_raw(p) })
    }
}

/// Convert `ptr` into a reference, or return a null pointer exception.
///
/// # Safety
///
/// If `ptr` is not null, it must be a valid pointer to an instance of `T`.
/// The underlying T must not be modified for so long as this reference exists.
///
/// (These are the same as the rules for `const *`s passed into the arti RPC lib.)
pub(super) unsafe fn ptr_as_ref<'a, T>(ptr: *const T) -> Result<&'a T, InvalidInput> {
    // Safety: we require that ptr, if set, is valid.
    unsafe { ptr.as_ref() }.ok_or(InvalidInput::NullPointer)
}

/// Helper for output parameters represented as `*mut *mut T`.
///
/// This is for an API which, from a C POV, returns an output via a parameter of type
/// `Foo **foo_out`.  If
///
/// The outer `foo_out` pointer may be NULL; if so, the caller wants to discard any values.
///
/// If `foo_out` is not NULL, then `*foo_out` is always set to NULL when an `OutPtr`
/// is constructed, so that even if the FFI code panics, the inner pointer will be initialized to
/// _something_.
pub(super) struct OutPtr<'a, T>(Option<&'a mut *mut T>);

impl<'a, T> OutPtr<'a, T> {
    /// Construct `Self` from a possibly NULL pointer; initialize `*ptr` to NULL if possible.
    ///
    /// # Safety
    ///
    /// The outer pointer, if set, must be valid, and must not alias any other pointers.
    ///
    /// See also the requirements on `pointer::as_mut()`.
    ///
    /// # No panics!
    ///
    /// This method can be invoked in cases where panicking is not allowed (such as
    /// in a FFI method, outside of `handle_errors()` or `catch_panic()`.)
    //
    // (I have tested this using the `no-panic` crate.  But `no-panic` is not suitable
    // for use in production, since it breaks when run in debug mode.)
    pub(super) unsafe fn from_opt_ptr(ptr: *mut *mut T) -> Self {
        if ptr.is_null() {
            OutPtr(None)
        } else {
            // TODO: Use `.as_mut_uninit` once it is stable.
            //
            // SAFETY: See documentation for [`<*mut *mut T>::as_uninit_mut`]
            // at https://doc.rust-lang.org/std/primitive.pointer.html#method.as_uninit_mut :
            // This is the same code.
            let ptr: &mut MaybeUninit<*mut T> = unsafe { &mut *(ptr as *mut MaybeUninit<*mut T>) };
            let ptr: &mut *mut T = ptr.write(std::ptr::null_mut());
            OutPtr(Some(ptr))
        }
    }

    /// As [`Self::from_opt_ptr`], but return an error if the pointer is NULL.
    ///
    /// This is appropriate in cases where it makes no sense to call the FFI function
    /// if you are going to throw away the output immediately.
    ///
    /// # Safety
    ///
    /// See [Self::from_opt_ptr].
    pub(super) unsafe fn from_ptr_nonnull(ptr: *mut *mut T) -> Result<Self, InvalidInput> {
        // Safety: We require that the pointer be valid for use with from_ptr.
        let r = unsafe { Self::from_opt_ptr(ptr) };
        if r.0.is_none() {
            Err(InvalidInput::NullPointer)
        } else {
            Ok(r)
        }
    }

    /// Consume this OutPtr and the provided value.
    ///
    /// If the OutPtr is null, `value` is discarded.  Otherwise it is written into the OutPtr.
    pub(super) fn write_value_if_nonnull(self, value: T) {
        // Note that all the unsafety happened when we constructed a &mut from the pointer.
        //
        // Note also that this method consumes `self`.  That's because we want to avoid multiple
        // writes to the same OutPtr: If we did that, we would sometimes have to free a previous
        // value.
        if let Some(ptr) = self.0 {
            *ptr = Box::into_raw(Box::new(value));
        }
    }
}

/// Implement the body of an FFI function.
///
/// This macro is meant to be invoked as follows:
///
/// ```ignore
///     ffi_body_simple!(
///         {
///             [CONVERSIONS]
///         } in {
///             [BODY]
///         } on invalid {
///             [VALUE_ON_BAD_INPUT]
///         } on panic {
///             [VALUE_ON_PANIC]
///         }
///     )
/// ```
///
/// For example:
///
/// ```ignore
/// pub extern "C" fn arti_rpc_cook_meal(
///     recipe: *const Recipe,
///     special_ingredients: *const Ingredients,
///     n_guests: usize,
///     dietary_constraints: *const c_char,
///     food_out: *mut *mut DeliciousMeal,
/// ) -> usize {
///     ffi_body_simple!(
///         { // [CONVERSIONS]
///             let recipe: &Recipe [in_ptr_required];
///             let ingredients: Option<&Ingredients> [in_ptr_opt];
///             let dietary_constraints: &str [in_str_required];
///             let food_out: OutPtr<DeliciousMeal> [out_ptr_opt];
///         } in {
///             // [BODY]
///             let delicious_meal = prepare_meal(recipe, ingredients, dietary_constraints, n_guests);
///             food_out.write_value_if_nonnull(delicious_meal);
///             n_guests as isize
///         } on invalid {
///             // [VALUE_ON_BAD_INPUT]
///             0
///         } on panic {
///             // [VALUE_ON_PANIC]
///             0
///         }
///     )
/// }
/// ```
///
/// The first part (`CONVERSIONS`) defines a set of conversions to be done on the function inputs.
/// These are documented below.
/// Each conversion performs an unsafe operation,
/// making certain assumptions about an input variable,
/// in order to produce an output of the specified type.
/// Conversions can reject input values.
/// If they do, the function will return;
/// see discussion of `[VALUE_ON_BAD_INPUT]`
///
/// The second part (`BODY`) is the body of the function.
/// It should generally be possible to write this body without using unsafe code.
/// The result of this block is the returned value of the function.
///
/// The third part (`VALUE_ON_BAD_INPUT`) is an expression to be returned
/// as the result of the function if any input pointer is NULL
/// that is not permitted to be NULL.
///
/// The fourth part (`VALUE_ON_PANIC`) is an expression to be returned
/// as the result of the function if any conversion
/// (or the body of the function) causes a panic.
///
/// ## Supported conversions
///
/// All conversions take the following format:
///
/// `let NAME : TYPE [METHOD] ;`
///
/// The `NAME` must match one of the inputs to the function.
///
/// The `TYPE` must match the actual type that the input will be converted to.
/// (These types are generally easy to use ergonomically from safe rust.)
///
/// The `METHOD` is an identifier explaining how the input is to be converted.
///
/// The following methods are recognized:
///
/// | method               | input type      | converted to     | can reject input? |
/// |----------------------|-----------------|------------------|-------------------|
/// | `in_ptr_required`    | `*const T`      | `&T`             | Y                 |
/// | `in_ptr_opt`         | `*const T`      | `Option<&T>`     | N                 |
/// | `in_str_required`    | `*const c_char` | `&str`           | Y                 |
/// | `in_ptr_consume_opt` | `*mut T`        | `Option<Box<T>>` | N                 |
/// | `out_ptr_required`   | `*mut *mut T`   | `OutPtr<T>`      | Y                 |
/// | `out_ptr_opt`        | `*mut *mut T`   | `OutPtr<T>`      | N                 |
///
/// > (Note: Other conversion methods are logically possible, but have not been added yet,
/// > since they would not yet be used in this crate.)
///
/// ## Safety
///
/// The `in_ptr_required` and `in_ptr_opt` methods
/// have the safety requirements of
/// [`<*const T>::as_ref`](https://doc.rust-lang.org/std/primitive.pointer.html#method.as_ref).
/// Informally, this means:
/// * If the pointer is not null, it must point
///   to a valid aligned dereferenceable instance of `T`.
/// * The underlying `T` must not be freed or modified for so long as the function is running.
///
/// The `in_str_required` method, when its input is non-NULL,
/// has the safety requirements of [`CStr::from_ptr`].
/// Informally, this means:
///  * If the pointer is not null, it must point to a nul-terminated string.
///  * The string must not be freed or modified for so long as the function is running.
///
/// Additionally, the `[in_str_required]` method
/// will detect invalid any string that is not UTF-8.
///
/// The `in_ptr_consume_opt` method, when its input is non-NULL,
/// has the safety requirements of [`Box::from_raw`].
/// Informally, this is satisfied when:
///  * If the pointer is not null, it should be
///    the result of an earlier a call to `Box<T>::into_raw`.
///    (Note that using either `out_ptr_*` method
///    will output pointers that can later be consumed in this way.)
///
/// The `out_ptr_required` and `out_ptr_opt` methods
/// have the safety requirements of
/// [`<*mut *mut T>::as_uninit_mut`](https://doc.rust-lang.org/std/primitive.pointer.html#method.as_uninit_mut).
/// Informally, this means:
///   * If the pointer (call it "out") is non-NULL, then `*out` must point to aligned
///     "dereferenceable" (q.v.) memory holding a possibly uninitialized "*mut T".
///
/// (Note that immediately upon conversion, if `out` is non-NULL,
/// `*out` is set to NULL.  See documentation for `OptPtr`.)
//
// Design notes:
// - I am keeping the conversions separate from the body below, since we don't want to catch
//   InvalidInput from the body.
// - The "on invalid" and "on panic" values must be specified explicitly,
//   since in general we should force the caller to think about them.
//   Getting a 0 or -1 wrong here can have nasty results.
// - The conversion syntax deliberately includes the type of the converted argument,
//   on the theory that it makes the functions more readable.
// - The conversion code deliberately shadows the original parameter with the
//   converted parameter.
macro_rules! ffi_body_simple {
    { { $( let $name:ident : $type:ty [$how:ident]);* $(;)? }
      in { $($body:tt)+ } on invalid { $err:expr } on panic { $panic:expr } } => {

        crate::ffi::err::catch_panic(|| {

            // run conversions and check for invalid input exceptions.
            crate::ffi::util::ffi_initialize!{ { $( let $name : $type [$how]; )* } else with _ignore_err {
                #[allow(clippy::unused_unit)]
                return $err;
            }};

            $($body)+

            },
            || { $panic }
        )
    }
}
pub(super) use ffi_body_simple;

/// Implement the body of an FFI function that returns an ArtiRpcStatus.
///
/// This macro is meant to be invoked as follows:
/// ```text
/// ffi_body_with_err!(
///         {
///             [CONVERSIONS]
///             err [ERRNAME] : OutPtr<ArtiRpcError>;
///         } in {
///             [BODY]
///         }
/// })```
///
/// For example:
///
/// ```ignore
/// pub extern "C" fn arti_rpc_wombat_feed(
///     wombat: *const Wombat,
///     wombat_chow: *const Meal,
///     error_out: *mut *mut ArtiRpcError
/// ) -> ArtiRpcStatus {
///     ffi_body_with_err!(
///         {
///             let wombat: &Wombat [in_ptr_required];
///             let wombat_chow: &Meal [in_ptr_required];
///             err error_out: OutPtr<ArtiRpcError>;
///         } in {
///             wombat.please_enjoy(wombat_chow)?;
///         }
///     )
/// }
/// ```
///
/// The resulting function has the same kinds
/// of conversions as would [`ffi_body_simple!`].
///
/// The differences are:
///   * Instead of returning a value, the body can only give errors with `?`.
///   * The function must return ArtiRpcStatus.
///   * Any errors that occur during the conversions or the body
///     are converted into an ArtiRpcError,
///     and given to the user via `error_out` if it is non-NULL.
///     A corresponding ArtiRpcStatus is returned.
///
/// ## Safety
///
/// The safety requirements for the `err` conversion
/// are the same as those for `out_ptr_opt` (q.v.).
macro_rules! ffi_body_with_err {
    { { $( let $name:ident : $type:ty [$how:ident]; )*
          err $err_out:ident : $err_type:ty $(;)? }
      in {$($body:tt)+}
    } => {{
        // (This is equivalent to using `out_ptr_opt`, but makes it more clear that the conversion
        // will never fail, and so we won't exit early.)
        let $err_out: $err_type = unsafe { crate::ffi::util::OutPtr::from_opt_ptr($err_out) };

        crate::ffi::err::handle_errors($err_out,
            || {
                crate::ffi::util::ffi_initialize!{ { $( let $name : $type [$how]; )* } else with err {
                    return Err(crate::ffi::err::ArtiRpcError::from(err));
                }};

                let () = { $($body)+ };

                Ok(())
            }
        )
    }}
}
pub(super) use ffi_body_with_err;

/// Implement a set of conversions, trying each one.
///
/// (It's important that this cannot exit early,
/// since some conversions have side effects: notably, the ones that create an OutPtr
/// can initialize that pointer to NULL, and we want to do that unconditionally.
///
/// If any conversion fails, run `return ($on_invalid)(error)` _after_ trying every conversion.
macro_rules! ffi_initialize {
    {
        { $( let $name:ident : $type:ty [$how:ident] ; )* } else with $err_id:ident { $($on_invalid:tt)* }
    } => {
        #[allow(unused_parens)]
        let ($($name,)*) : ($($type,)*) = {
            $(
                let $name : Result<$type, crate::ffi::err::InvalidInput>
                   = crate::ffi::util::ffi_initialize!{ @init $name [$how] };
            )*
            #[allow(clippy::needless_question_mark)]
            // Note that the question marks here exit from _this_ closure.
            match (|| -> Result<_,crate::ffi::err::InvalidInput> {Ok(($($name?,)*))})() {
                Ok(v) => v,
                Err($err_id) => {
                    $($on_invalid)*
                }
            }
        };
    };
    // Each of these conversions should evaluate `Err(InvalidInput)` if the conversion fails,
    // and `Ok($ty)` if the conversion succeeds.
    // (Infallible conversions always return `Ok`.)
    // A conversion must not return early (via return or `?`).
    { @init $name:ident [in_ptr_required] } => {
        unsafe { crate::ffi::util::ptr_as_ref($name) }
    };
    { @init $name:ident [in_ptr_opt] } => {
        Ok(unsafe { crate::ffi::util::ptr_as_ref($name) })
    };
    { @init $name:ident [in_str_required] } => {
        unsafe { crate::ffi::util::ptr_to_str($name) }
    };
    { @init $name:ident [in_ptr_consume_opt] } => {
        Ok(unsafe { crate::ffi::util::ptr_to_opt_box($name) })
    };
    { @init $name:ident [out_ptr_required] } => {
        unsafe { crate::ffi::util::OutPtr::from_ptr_nonnull($name) }
    };
    { @init $name:ident [out_ptr_opt] } => {
        Ok(unsafe { crate::ffi::util::OutPtr::from_opt_ptr($name) })
    };
}
pub(super) use ffi_initialize;

#[cfg(test)]
mod test {
    // @@ begin test lint list maintained by maint/add_warning @@
    #![allow(clippy::bool_assert_comparison)]
    #![allow(clippy::clone_on_copy)]
    #![allow(clippy::dbg_macro)]
    #![allow(clippy::mixed_attributes_style)]
    #![allow(clippy::print_stderr)]
    #![allow(clippy::print_stdout)]
    #![allow(clippy::single_char_pattern)]
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::unchecked_duration_subtraction)]
    #![allow(clippy::useless_vec)]
    #![allow(clippy::needless_pass_by_value)]
    //! <!-- @@ end test lint list maintained by maint/add_warning @@ -->

    use super::*;

    unsafe fn outptr_user(ptr: *mut *mut i8, set_to_val: Option<i8>) {
        let ptr = unsafe { OutPtr::from_opt_ptr(ptr) };

        if let Some(v) = set_to_val {
            ptr.write_value_if_nonnull(v);
        }
    }

    #[test]
    fn outptr() {
        let mut ptr_to_int: *mut i8 = 7 as _; // This is a junk dangling pointer.  It will get overwritten.

        // Case 1: Don't set to anything.
        unsafe { outptr_user(&mut ptr_to_int as _, None) };
        assert!(ptr_to_int.is_null());

        // Cases 2, 3: Provide a null pointer for the output pointer.
        ptr_to_int = 7 as _; // make it junk again.
        unsafe { outptr_user(std::ptr::null_mut(), None) };
        assert_eq!(ptr_to_int, 7 as _); // we didn't pass this in, so it wasn't set.
        unsafe { outptr_user(std::ptr::null_mut(), Some(5)) };
        assert_eq!(ptr_to_int, 7 as _); // we didn't pass this in, so it wasn't set.

        // Case 4: Actually set something.
        unsafe { outptr_user(&mut ptr_to_int as _, Some(123)) };
        assert!(!ptr_to_int.is_null());
        let boxed = unsafe { Box::from_raw(ptr_to_int) };
        assert_eq!(*boxed, 123);
    }

    unsafe fn outptr_user_nn(
        ptr: *mut *mut i8,
        set_to_val: Option<i8>,
    ) -> Result<(), InvalidInput> {
        let ptr = unsafe { OutPtr::from_ptr_nonnull(ptr) }?;

        if let Some(v) = set_to_val {
            ptr.write_value_if_nonnull(v);
        }
        Ok(())
    }

    #[test]
    fn outptr_nonnull() {
        let mut ptr_to_int: *mut i8 = 7 as _; // This is a junk dangling pointer.  It will get overwritten.

        // Case 1: Don't set to anything.
        let r = unsafe { outptr_user_nn(&mut ptr_to_int as _, None) };
        assert!(r.is_ok());
        assert!(ptr_to_int.is_null());

        // Cases 2, 3: Provide a null pointer for the output pointer.
        ptr_to_int = 7 as _; // make it junk again.
        let r = unsafe { outptr_user_nn(std::ptr::null_mut(), None) };
        assert!(r.is_err());
        assert_eq!(ptr_to_int, 7 as _); // we didn't pass this in, so it wasn't set.
        let r = unsafe { outptr_user_nn(std::ptr::null_mut(), Some(5)) };
        assert!(r.is_err());
        assert_eq!(ptr_to_int, 7 as _); // we didn't pass this in, so it wasn't set.

        // Case 4: Actually set something.
        let r = unsafe { outptr_user_nn(&mut ptr_to_int as _, Some(123)) };
        assert!(r.is_ok());
        assert!(!ptr_to_int.is_null());
        let boxed = unsafe { Box::from_raw(ptr_to_int) };
        assert_eq!(*boxed, 123);
    }
}
