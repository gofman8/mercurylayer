# Set the minimum required version of CMake
cmake_minimum_required(VERSION 3.15)

# Define the project name and language
project(MercuryLockbox VERSION 0.1
                    DESCRIPTION "Mercury Lockbox"
                    LANGUAGES C CXX)

# Add C compiler settings
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

# Specify C++ standard
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Add the executable and source files
add_executable(MercuryLockbox src/main.cpp)

# Include directories (e.g., header files in include/)
# target_include_directories(MercuryLockbox PRIVATE include)

# Add the cmake directory to the module path
list(APPEND CMAKE_MODULE_PATH ${CMAKE_CURRENT_SOURCE_DIR}/cmake)

# Include the secp256k1_zkp configuration
include(secp256k1_zkp)

# [BUILD] Google KMS is OPT-IN, and off by default.
#
# `google-cloud-cpp` drags in grpc, protobuf, abseil and re2. On arm64-linux that chain FAILS to
# build under vcpkg ("error: building grpc:arm64-linux failed"), which made the whole lockbox image
# unbuildable on an ARM host — including for people who never use Google KMS. The deployed
# compose file selects `KEY_MANAGER=hashicorp_container`, so nothing in the shipping path touches
# it: the dependency was costing everyone a broken build to serve one unused branch.
#
# Enable with `-DWITH_GOOGLE_KMS=ON` (and the matching vcpkg feature) if you actually deploy on KMS.
option(WITH_GOOGLE_KMS "Build the Google Cloud KMS key manager (pulls grpc/protobuf/abseil)" OFF)

set(LOCKBOX_SOURCES
    src/server.cpp
    src/utils.cpp
    src/hashicorp_api_key_manager.cpp
    src/hashicorp_container_key_manager.cpp
    src/filesystem_key_manager.cpp
    src/enclave.cpp
    src/monocypher.c
    src/db_manager.cpp)

if(WITH_GOOGLE_KMS)
    list(APPEND LOCKBOX_SOURCES src/google_key_manager.cpp)
endif()

add_library(LockboxLibrary ${LOCKBOX_SOURCES})

set_source_files_properties(src/monocypher.c PROPERTIES LANGUAGE C)

target_include_directories(LockboxLibrary PUBLIC include)

# Ensure proper C/C++ linkage
target_compile_options(LockboxLibrary PRIVATE
    $<$<COMPILE_LANGUAGE:C>:-std=c11>
    $<$<COMPILE_LANGUAGE:CXX>:-std=c++17>)
    
target_link_libraries(LockboxLibrary
    Crow::Crow
    cpr::cpr
    pqxx
    tomlplusplus::tomlplusplus
    secp256k1_zkp
    OpenSSL::SSL
    OpenSSL::Crypto)

if(WITH_GOOGLE_KMS)
    target_compile_definitions(LockboxLibrary PUBLIC WITH_GOOGLE_KMS)
    target_link_libraries(LockboxLibrary
        google-cloud-cpp::storage
        google-cloud-cpp::kms
        google-cloud-cpp::secretmanager)
endif()

include(FetchContent)
FetchContent_Declare(
    tomlplusplus
    GIT_REPOSITORY https://github.com/marzer/tomlplusplus.git
    GIT_TAG v3.4.0
)
FetchContent_MakeAvailable(tomlplusplus)

FetchContent_Declare(
    libpqxx
    GIT_REPOSITORY https://github.com/jtv/libpqxx.git
    GIT_TAG        7.8.1
)

FetchContent_MakeAvailable(libpqxx)

find_package(Crow)
find_package(cpr REQUIRED)
if(WITH_GOOGLE_KMS)
    find_package(google_cloud_cpp_storage REQUIRED)
    find_package(google_cloud_cpp_kms REQUIRED)
    find_package(google_cloud_cpp_secretmanager REQUIRED)
endif()
find_package(OpenSSL REQUIRED)

target_include_directories(MercuryLockbox PRIVATE include)
# Link the library to the main executable
target_link_libraries(MercuryLockbox PRIVATE
    LockboxLibrary
    Crow::Crow
    cpr::cpr
    pqxx
    tomlplusplus::tomlplusplus
    secp256k1_zkp
    OpenSSL::SSL
    OpenSSL::Crypto)

if(WITH_GOOGLE_KMS)
    target_link_libraries(MercuryLockbox PRIVATE
        google-cloud-cpp::storage
        google-cloud-cpp::kms
        google-cloud-cpp::secretmanager)
endif()

# Copy Settings.toml after building MercuryLockbox
# add_custom_command(TARGET MercuryLockbox POST_BUILD
# COMMAND ${CMAKE_COMMAND} -E copy_if_different
# ${CMAKE_SOURCE_DIR}/Settings.toml
# $<TARGET_FILE_DIR:MercuryLockbox>/Settings.toml)
