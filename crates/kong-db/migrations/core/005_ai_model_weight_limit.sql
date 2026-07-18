ALTER TABLE ai_models
    ADD CONSTRAINT ai_models_weight_range
    CHECK (weight BETWEEN 0 AND 10000);
