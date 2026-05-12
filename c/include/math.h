#ifndef IC_SQLITE_MATH_H
#define IC_SQLITE_MATH_H

#define HUGE_VAL (__builtin_huge_val())

static inline double fabs(double x) { return x < 0 ? -x : x; }
static inline int isnan(double x) { return x != x; }
static inline int isinf(double x) { return !isnan(x) && isnan(x - x); }
static inline int finite(double x) { return !isnan(x) && !isinf(x); }
double log(double x);

#endif
