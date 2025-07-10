
#include<iostream>

extern "C" {
int testcxx() {
    std::cout << "Hello, World!" << std::endl;
    return 0;
}
}
