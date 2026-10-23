cmake_minimum_required(VERSION 3.16)
project(minic LANGUAGES CXX)

add_executable(minic
    src/main.cpp
    src/lexer.cpp
    src/parser.cpp
)
target_compile_features(minic PRIVATE cxx_std_17)
target_include_directories(minic PRIVATE src)

enable_testing()
add_test(NAME precedence COMMAND minic --ast ${CMAKE_CURRENT_SOURCE_DIR}/examples/precedence.mc)
set_tests_properties(precedence PROPERTIES PASS_REGULAR_EXPRESSION "\\(return \\(\\+ 1 \\(\\* 2 3\\)\\)\\)")
