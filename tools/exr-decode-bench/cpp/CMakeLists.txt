cmake_minimum_required(VERSION 3.16)
project(openexr_bench CXX)
set(CMAKE_CXX_STANDARD 17)
# Point CMAKE_PREFIX_PATH at the OpenEXR install to benchmark against.
find_package(OpenEXR CONFIG REQUIRED)
add_executable(openexr-bench openexr_bench.cpp)
target_link_libraries(openexr-bench PRIVATE OpenEXR::OpenEXR)
