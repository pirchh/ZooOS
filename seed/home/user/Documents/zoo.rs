// A small program, to show Notes' colours.
use core::fmt::Write;

#[derive(Debug)]
struct Animal {
    name: &'static str,
    legs: u32,
}

/* Every animal in the zoo. */
fn main() {
    let zoo = [Animal { name: "Peacock", legs: 2 }, Animal { name: "Badger", legs: 4 }];
    for a in zoo.iter() {
        println!("{} has {} legs", a.name, a.legs);
    }
}
