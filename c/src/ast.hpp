#pragma once

#include <cstdint>
#include <memory>
#include <string>

struct Expr {
    enum class Kind { Integer, Unary, Binary } kind;
    std::int64_t integer = 0;
    char op = '\0';
    std::unique_ptr<Expr> left;
    std::unique_ptr<Expr> right;
};

std::string print_ast(const Expr& expr);
