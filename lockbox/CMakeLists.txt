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
    src/db_manager.cpp
    src/tx.cpp
    src/witness.cpp
    src/auth.cpp
    src/registry.cpp)

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

# ── DIFFERENTIAL TESTS ────────────────────────────────────────────────────────────────────────────
#
# These two files existed as standalone `main()` programs with NO build target, runnable only by
# hand-compiling them inside a container against the vendored secp256k1-zkp. That is not a
# theoretical gap: it is exactly how a STALE binary once reported "3/3 pass" for a change that had
# not compiled at all — the compiler error was filtered away, the previous build was still on disk,
# and its output was read as a pass. A target removes the opportunity.
#
# They are wired to RUN AT BUILD TIME (POST_BUILD), not merely to compile, because the Docker image
# build runs `cmake --build .` and nothing else. A test that only builds in CI is a test nobody runs
# on the path that produces the shipping artifact — so a mismatch between the SE's reconstruction and
# the client's would reach a deployed lockbox and refuse every honest signature.
#
# Both link only what they need (tx.cpp pulls OpenSSL for SHA-256; the session differential needs
# just secp256k1_zkp) rather than the whole LockboxLibrary, so a failure here cannot be confused with
# a Crow/pqxx/cpr build problem.
option(BUILD_LOCKBOX_TESTS "Build and run the lockbox differential tests" ON)
if(BUILD_LOCKBOX_TESTS)
    enable_testing()

    # BIP-341 sighash + transaction parser, against vectors generated by the CLIENT's own builders
    # (`cargo test -p ci-guards --test emit_sighash_vectors`).
    add_executable(test_tx_sighash tests/test_tx_sighash.cpp src/tx.cpp)
    target_include_directories(test_tx_sighash PRIVATE include tests)
    target_link_libraries(test_tx_sighash PRIVATE secp256k1_zkp OpenSSL::Crypto)
    add_test(NAME tx_sighash COMMAND test_tx_sighash)
    add_custom_command(TARGET test_tx_sighash POST_BUILD
        COMMAND test_tx_sighash
        COMMENT "Running the BIP-341 sighash differential")

    # Blinded-session reconstruction, against the WIRE form the client sends
    # (`remove_fin_nonce_from_session().serialize()`, 133 bytes).
    add_executable(test_session_rebuild tests/test_session_rebuild.cpp)
    target_include_directories(test_session_rebuild PRIVATE include tests)
    target_link_libraries(test_session_rebuild PRIVATE secp256k1_zkp)
    add_test(NAME session_rebuild COMMAND test_session_rebuild)
    add_custom_command(TARGET test_session_rebuild POST_BUILD
        COMMAND test_session_rebuild
        COMMENT "Running the blinded-session reconstruction differential")

    # BIP-340 verification — the SE's FIRST signature check, against vectors signed by the client
    # (`cargo test -p ci-guards --test emit_bip340_vectors`). Every positive case is paired with a
    # control that must fail, because a verifier that accepts everything passes positives alone.
    add_executable(test_bip340 tests/test_bip340.cpp src/auth.cpp src/tx.cpp)
    target_include_directories(test_bip340 PRIVATE include tests)
    target_link_libraries(test_bip340 PRIVATE secp256k1_zkp OpenSSL::Crypto)
    add_test(NAME bip340 COMMAND test_bip340)
    add_custom_command(TARGET test_bip340 POST_BUILD
        COMMAND test_bip340
        COMMENT "Running the BIP-340 verification differential")

    # REQ-56's predicate. Pure arithmetic over a leaf set, so every case that matters — a fork, a
    # released sibling, a shared exit key, a one-satoshi shortfall — is reachable here and NONE of
    # them is reachable cheaply through a live stack. A permissive predicate is silent: the honest
    # path keeps working and the only symptom is an unpaid absentee with no recourse.
    add_executable(test_registry tests/test_registry.cpp src/registry.cpp src/tx.cpp)
    target_include_directories(test_registry PRIVATE include tests)
    target_link_libraries(test_registry PRIVATE OpenSSL::Crypto)
    add_test(NAME registry COMMAND test_registry)
    add_custom_command(TARGET test_registry POST_BUILD
        COMMAND test_registry
        COMMENT "Running the REQ-56 collapse-predicate tests")

    # txid + structural payload selection, against vectors from bitcoin's own Transaction::txid()
    # — including a WITNESS-BEARING transaction, the case that separates txid from wtxid.
    add_executable(test_txid_payload tests/test_txid_payload.cpp src/tx.cpp src/witness.cpp
                                     src/utils.cpp src/monocypher.c)
    target_include_directories(test_txid_payload PRIVATE include tests)
    target_link_libraries(test_txid_payload PRIVATE
        Crow::Crow tomlplusplus::tomlplusplus secp256k1_zkp OpenSSL::SSL OpenSSL::Crypto)
    add_test(NAME txid_payload COMMAND test_txid_payload)
    add_custom_command(TARGET test_txid_payload POST_BUILD
        COMMAND test_txid_payload
        COMMENT "Running the txid + payload-selection differential")

    # The registry against a REAL Postgres. Deliberately NOT run POST_BUILD: there is no database
    # during `docker build`, and a test that skips when its dependency is missing is worse than no
    # test — it reports success for something it never ran. Built into the image; run explicitly:
    #     docker exec mercurylayer-lockbox-1 /app/build/test_registry_db
    add_executable(test_registry_db tests/test_registry_db.cpp src/registry.cpp src/tx.cpp
                                    src/db_manager.cpp src/utils.cpp src/monocypher.c)
    target_include_directories(test_registry_db PRIVATE include tests)
    target_link_libraries(test_registry_db PRIVATE
        pqxx tomlplusplus::tomlplusplus secp256k1_zkp OpenSSL::SSL OpenSSL::Crypto)
endif()

# Copy Settings.toml after building MercuryLockbox
# add_custom_command(TARGET MercuryLockbox POST_BUILD
# COMMAND ${CMAKE_COMMAND} -E copy_if_different
# ${CMAKE_SOURCE_DIR}/Settings.toml
# $<TARGET_FILE_DIR:MercuryLockbox>/Settings.toml)
