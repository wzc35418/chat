#include "parser.hpp"

#include <stdexcept>
#include <utility>

namespace {
std::unique_ptr<Expr> binary(char op, std::unique_ptr<Expr> left,
                             std::unique_ptr<Expr> right) {
    auto expr = std::make_unique<Expr>();
    expr->kind = Expr::Kind::Binary;
    expr->op = op;
    expr->left = std::move(left);
    expr->right = std::move(right);
    return expr;
}
}

Parser::Parser(const std::vector<Token>& tokens, std::string filename)
    : tokens_(tokens), filename_(std::move(filename)) {}

const Token& Parser::current() const { return tokens_.at(position_); }

bool Parser::take(TokenKind kind) {
    if (current().kind != kind) return false;
    ++position_;
    return true;
}

void Parser::expect(TokenKind kind) {
    if (take(kind)) return;
    const Token& token = current();
    throw std::runtime_error(filename_ + ":" + std::to_string(token.line) + ":" +
        std::to_string(token.column) + ": expected " + token_name(kind) +
        ", got " + token_name(token.kind));
}

std::unique_ptr<Expr> Parser::parse_return() {
    expect(TokenKind::Return);
    auto value = parse_expression();
    expect(TokenKind::Semicolon);
    expect(TokenKind::End);
    return value;
}

std::unique_ptr<Expr> Parser::parse_expression() {
    auto left = parse_term();
    while (current().kind == TokenKind::Plus || current().kind == TokenKind::Minus) {
        const char op = current().text[0];
        ++position_;
        left = binary(op, std::move(left), parse_term());
    }
    return left;
}

std::unique_ptr<Expr> Parser::parse_term() {
    auto left = parse_unary();
    while (current().kind == TokenKind::Star || current().kind == TokenKind::Slash) {
        const char op = current().text[0];
        ++position_;
        left = binary(op, std::move(left), parse_unary());
    }
    return left;
}

std::unique_ptr<Expr> Parser::parse_unary() {
    if (take(TokenKind::Minus)) {
        auto expr = std::make_unique<Expr>();
        expr->kind = Expr::Kind::Unary;
        expr->op = '-';
        expr->right = parse_unary();
        return expr;
    }
    return parse_primary();
}

std::unique_ptr<Expr> Parser::parse_primary() {
    if (current().kind == TokenKind::Integer) {
        auto expr = std::make_unique<Expr>();
        expr->kind = Expr::Kind::Integer;
        expr->integer = current().integer;
        ++position_;
        return expr;
    }
    if (take(TokenKind::LeftParen)) {
        auto expr = parse_expression();
        expect(TokenKind::RightParen);
        return expr;
    }
    const Token& token = current();
    throw std::runtime_error(filename_ + ":" + std::to_string(token.line) + ":" +
        std::to_string(token.column) + ": expected integer or '(', got " +
        token_name(token.kind));
}

std::string print_ast(const Expr& expr) {
    switch (expr.kind) {
    case Expr::Kind::Integer:
        return std::to_string(expr.integer);
    case Expr::Kind::Unary:
        return "(" + std::string(1, expr.op) + " " + print_ast(*expr.right) + ")";
    case Expr::Kind::Binary:
        return "(" + std::string(1, expr.op) + " " + print_ast(*expr.left) +
            " " + print_ast(*expr.right) + ")";
    }
    return "";
}
