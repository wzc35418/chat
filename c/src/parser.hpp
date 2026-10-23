#pragma once

#include "ast.hpp"
#include "token.hpp"

#include <memory>
#include <string>
#include <vector>

class Parser {
public:
    Parser(const std::vector<Token>& tokens, std::string filename);
    std::unique_ptr<Expr> parse_return();

private:
    const Token& current() const;
    bool take(TokenKind kind);
    void expect(TokenKind kind);
    std::unique_ptr<Expr> parse_expression();
    std::unique_ptr<Expr> parse_term();
    std::unique_ptr<Expr> parse_unary();
    std::unique_ptr<Expr> parse_primary();

    const std::vector<Token>& tokens_;
    std::string filename_;
    std::size_t position_ = 0;
};
