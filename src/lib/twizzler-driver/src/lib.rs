#![feature(option_result_unwrap_unchecked)]
#![feature(generic_associated_types)]

pub mod bus;
pub mod device;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
