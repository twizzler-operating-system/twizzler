
#include<iostream>

extern "C" {
int testcxx() {
    auto x = new int(3);
    std::cout << "Hello, World! " << x << ", " << *x << std::endl;
    return 0;
}
}
